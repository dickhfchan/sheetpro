//! Pivot-table engine: item lists, aggregation, and the layout of the output block.
//! Pure functions over `sp_model::pivot::PivotTable` (its cache snapshot); used by the pivot
//! commands (refresh writes the block into cells), GETPIVOTDATA, and consolidation.

use sp_core::{ErrorKind, MAX_CELLS_PER_OP, MAX_COLS, MAX_ROWS, Value};
use sp_model::pivot::{Aggregate, PivotCache, PivotLayout, PivotTable, ShowAs};
use std::cmp::Ordering;
use std::collections::HashMap;

// ---------------------------------------------------------------------------------------
// Items

/// Grouping key of a value: numbers by bits, text case-insensitively.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ItemKey {
    Num(u64),
    Text(String),
    Bool(bool),
    Err(String),
    Blank,
}

pub fn item_key(v: &Value) -> ItemKey {
    match v {
        Value::Number(n) => ItemKey::Num(if *n == 0.0 { 0 } else { n.to_bits() }),
        Value::Text(t) if t.is_empty() => ItemKey::Blank,
        Value::Text(t) => ItemKey::Text(t.to_lowercase()),
        Value::Bool(b) => ItemKey::Bool(*b),
        Value::Error(e) => ItemKey::Err(e.as_str().to_string()),
        Value::Empty => ItemKey::Blank,
    }
}

fn rank(v: &Value) -> u8 {
    match v {
        Value::Number(_) => 0,
        Value::Text(t) if !t.is_empty() => 1,
        Value::Bool(_) => 2,
        Value::Error(_) => 3,
        _ => 4,
    }
}

/// Default item order: numbers ascending, then text (case-insensitive), logicals, errors,
/// blanks last.
pub fn cmp_items(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Text(x), Value::Text(y)) if !x.is_empty() && !y.is_empty() => {
            x.to_lowercase().cmp(&y.to_lowercase())
        }
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// Display text of an item (`(blank)` for empty).
pub fn item_label(v: &Value) -> Value {
    match v {
        Value::Empty => Value::text("(blank)"),
        Value::Text(t) if t.is_empty() => Value::text("(blank)"),
        other => other.clone(),
    }
}

/// The sorted distinct items of one field, minus hidden ones.
#[derive(Debug, Clone, Default)]
pub struct FieldItems {
    pub items: Vec<Value>,
    index: HashMap<ItemKey, u32>,
}

impl FieldItems {
    pub fn index_of(&self, v: &Value) -> Option<u32> {
        self.index.get(&item_key(v)).copied()
    }
}

/// Every distinct item of `field` in `cache` (sorted), hidden ones excluded.
pub fn field_items(cache: &PivotCache, field: usize, hidden: &[Value]) -> FieldItems {
    let hidden: std::collections::HashSet<ItemKey> = hidden.iter().map(item_key).collect();
    let mut seen: HashMap<ItemKey, Value> = HashMap::new();
    for r in &cache.records {
        let v = r.get(field).cloned().unwrap_or(Value::Empty);
        let k = item_key(&v);
        if !hidden.contains(&k) {
            seen.entry(k).or_insert(v);
        }
    }
    let mut items: Vec<Value> = seen.into_values().collect();
    items.sort_by(cmp_items);
    let index = items
        .iter()
        .enumerate()
        .map(|(i, v)| (item_key(v), i as u32))
        .collect();
    FieldItems { items, index }
}

/// Every distinct item of `field` (hidden ones included), sorted.
pub fn all_items(cache: &PivotCache, field: usize) -> Vec<Value> {
    field_items(cache, field, &[]).items
}

// ---------------------------------------------------------------------------------------
// Aggregation

/// Running summary of a group of values (all aggregates at once).
#[derive(Debug, Clone, Copy, Default)]
pub struct Acc {
    /// Non-empty values.
    pub count: u64,
    /// Numeric values.
    pub nums: u64,
    pub sum: f64,
    pub product: f64,
    pub min: f64,
    pub max: f64,
    mean: f64,
    m2: f64,
    /// First error value seen (sum/average/… of a group holding an error is that error).
    pub error: Option<ErrorKind>,
}

impl Acc {
    pub fn add(&mut self, v: &Value) {
        match v {
            Value::Empty => {}
            Value::Text(t) if t.is_empty() => {}
            Value::Number(n) => {
                self.count += 1;
                let n = *n;
                if self.nums == 0 {
                    self.min = n;
                    self.max = n;
                    self.product = n;
                } else {
                    self.min = self.min.min(n);
                    self.max = self.max.max(n);
                    self.product *= n;
                }
                self.nums += 1;
                self.sum += n;
                let d = n - self.mean;
                self.mean += d / self.nums as f64;
                self.m2 += d * (n - self.mean);
            }
            Value::Error(e) => {
                self.count += 1;
                if self.error.is_none() {
                    self.error = Some(*e);
                }
            }
            _ => self.count += 1,
        }
    }

    pub fn result(&self, f: Aggregate) -> Value {
        if f == Aggregate::Count {
            return Value::Number(self.count as f64);
        }
        if let Some(e) = self.error {
            return Value::Error(e);
        }
        let n = self.nums as f64;
        let r = match f {
            Aggregate::Sum => self.sum,
            Aggregate::Count => self.count as f64,
            Aggregate::CountNums => n,
            Aggregate::Average if self.nums == 0 => return Value::Error(ErrorKind::Div0),
            Aggregate::Average => self.sum / n,
            Aggregate::Max if self.nums == 0 => 0.0,
            Aggregate::Max => self.max,
            Aggregate::Min if self.nums == 0 => 0.0,
            Aggregate::Min => self.min,
            Aggregate::Product if self.nums == 0 => 0.0,
            Aggregate::Product => self.product,
            Aggregate::StdDev | Aggregate::Var if self.nums < 2 => {
                return Value::Error(ErrorKind::Div0);
            }
            Aggregate::StdDevP | Aggregate::VarP if self.nums < 1 => {
                return Value::Error(ErrorKind::Div0);
            }
            Aggregate::StdDev => (self.m2 / (n - 1.0)).max(0.0).sqrt(),
            Aggregate::Var => (self.m2 / (n - 1.0)).max(0.0),
            Aggregate::StdDevP => (self.m2 / n).max(0.0).sqrt(),
            Aggregate::VarP => (self.m2 / n).max(0.0),
        };
        crate::eval::num_value(r)
    }
}

/// Aggregate a list of values.
pub fn aggregate<'a>(values: impl IntoIterator<Item = &'a Value>, f: Aggregate) -> Value {
    let mut a = Acc::default();
    for v in values {
        a.add(v);
    }
    a.result(f)
}

// ---------------------------------------------------------------------------------------
// Cache building

/// Build a cache from a source block (header row first). Blank headers are refused like
/// the target does; duplicate names get a numeric suffix.
pub fn build_cache(block: &[Vec<Value>]) -> Result<PivotCache, String> {
    let Some(header) = block.first() else {
        return Err("The source reference is not valid.".into());
    };
    if header.is_empty() || header.len() > sp_model::pivot::MAX_FIELDS {
        return Err("The source reference is not valid.".into());
    }
    if block.len() > sp_model::pivot::MAX_RECORDS.saturating_add(1) {
        return Err("The source range has too many rows.".into());
    }
    let mut fields: Vec<String> = Vec::with_capacity(header.len());
    for v in header {
        let base = match v {
            Value::Empty => String::new(),
            other => other.to_display(),
        };
        if base.trim().is_empty() {
            return Err(
                "The PivotTable field name is not valid. Every column of the source needs a heading."
                    .into(),
            );
        }
        let mut name = base.clone();
        let mut k = 2u32;
        while fields.iter().any(|f| f.eq_ignore_ascii_case(&name)) && k < 100_000 {
            name = format!("{base}{k}");
            k += 1;
        }
        fields.push(name);
    }
    let width = fields.len();
    let records = block
        .iter()
        .skip(1)
        .map(|r| {
            let mut r = r.clone();
            r.resize(width, Value::Empty);
            r
        })
        .collect();
    Ok(PivotCache { fields, records })
}

// ---------------------------------------------------------------------------------------
// Layout

/// What a cell of the output block is (drives formatting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellKind {
    #[default]
    Blank,
    /// Header captions ("Row Labels", field names, data captions, filter names).
    Header,
    /// A row/column item label (with its compact-form indent level).
    Label(u8),
    /// A subtotal or grand-total label.
    TotalLabel,
    Value,
    /// A subtotal / grand-total value.
    TotalValue,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutCell {
    pub value: Value,
    pub kind: CellKind,
    /// Value shown as a percentage (show-values-as).
    pub percent: bool,
    /// Index of the data field (in `PivotTable::values`) a value cell shows.
    pub data: Option<usize>,
}

/// The computed output block.
#[derive(Debug, Clone, Default)]
pub struct PivotOutput {
    pub rows: usize,
    pub cols: usize,
    /// Row-major cells.
    pub cells: Vec<OutCell>,
    /// Rows of the report-filter area (filters + one blank row) above the table.
    pub page_rows: usize,
    /// Header rows of the table (first data row offset within the table).
    pub header_rows: usize,
    /// Label columns (first data column offset).
    pub label_cols: usize,
}

impl PivotOutput {
    pub fn get(&self, r: usize, c: usize) -> Option<&OutCell> {
        if r >= self.rows || c >= self.cols {
            return None;
        }
        self.cells.get(r * self.cols + c)
    }

    fn set(&mut self, r: usize, c: usize, value: Value, kind: CellKind) {
        if r < self.rows
            && c < self.cols
            && let Some(cell) = self.cells.get_mut(r * self.cols + c)
        {
            cell.value = value;
            cell.kind = kind;
        }
    }

    fn set_value(&mut self, r: usize, c: usize, value: Value, kind: CellKind, percent: bool, data: usize) {
        if r < self.rows
            && c < self.cols
            && let Some(cell) = self.cells.get_mut(r * self.cols + c)
        {
            cell.value = value;
            cell.kind = kind;
            cell.percent = percent;
            cell.data = Some(data);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum LineKind {
    Item,
    Subtotal,
    Grand,
}

/// One row (or column) of the table body: the key prefix it aggregates.
#[derive(Debug, Clone)]
struct Line {
    key: Vec<u32>,
    kind: LineKind,
}

type GroupKey = (Vec<u32>, Vec<u32>);

/// Records, item lists and accumulators of a pivot.
struct Prepared {
    row_items: Vec<FieldItems>,
    col_items: Vec<FieldItems>,
    /// Distinct full row keys / col keys present, sorted.
    row_keys: Vec<Vec<u32>>,
    col_keys: Vec<Vec<u32>>,
    accs: HashMap<GroupKey, Vec<Acc>>,
}

const MAX_GROUPS: usize = 5_000_000;

fn prepare(p: &PivotTable) -> Result<Prepared, String> {
    let cache = &p.cache;
    let nfields = cache.fields.len();
    for f in p
        .rows
        .iter()
        .chain(&p.cols)
        .chain(&p.filters)
        .chain(p.values.iter().map(|d| &d.field))
    {
        if *f >= nfields {
            return Err("pivot field index out of range".into());
        }
    }
    let row_items: Vec<FieldItems> = p
        .rows
        .iter()
        .map(|f| field_items(cache, *f, p.hidden_items(*f)))
        .collect();
    let col_items: Vec<FieldItems> = p
        .cols
        .iter()
        .map(|f| field_items(cache, *f, p.hidden_items(*f)))
        .collect();
    // Filters on fields that are not on an axis.
    let other_filters: Vec<(usize, std::collections::HashSet<ItemKey>)> = p
        .field_filters
        .iter()
        .filter(|ff| !ff.hidden.is_empty() && !p.rows.contains(&ff.field) && !p.cols.contains(&ff.field))
        .map(|ff| (ff.field, ff.hidden.iter().map(item_key).collect()))
        .collect();
    let nd = p.values.len();
    let mut accs: HashMap<GroupKey, Vec<Acc>> = HashMap::new();
    let mut row_set: HashMap<Vec<u32>, ()> = HashMap::new();
    let mut col_set: HashMap<Vec<u32>, ()> = HashMap::new();
    let empty = Value::Empty;
    'rec: for r in &cache.records {
        for (f, hidden) in &other_filters {
            if hidden.contains(&item_key(r.get(*f).unwrap_or(&empty))) {
                continue 'rec;
            }
        }
        let mut rk = Vec::with_capacity(p.rows.len());
        for (f, items) in p.rows.iter().zip(&row_items) {
            match items.index_of(r.get(*f).unwrap_or(&empty)) {
                Some(i) => rk.push(i),
                None => continue 'rec,
            }
        }
        let mut ck = Vec::with_capacity(p.cols.len());
        for (f, items) in p.cols.iter().zip(&col_items) {
            match items.index_of(r.get(*f).unwrap_or(&empty)) {
                Some(i) => ck.push(i),
                None => continue 'rec,
            }
        }
        if accs.len() > MAX_GROUPS {
            return Err("The PivotTable is too large.".into());
        }
        for k in 0..=rk.len() {
            for m in 0..=ck.len() {
                let key = (
                    rk.get(..k).unwrap_or(&[]).to_vec(),
                    ck.get(..m).unwrap_or(&[]).to_vec(),
                );
                let slot = accs.entry(key).or_insert_with(|| vec![Acc::default(); nd]);
                for (d, df) in p.values.iter().enumerate() {
                    if let Some(a) = slot.get_mut(d) {
                        a.add(r.get(df.field).unwrap_or(&empty));
                    }
                }
            }
        }
        row_set.insert(rk, ());
        col_set.insert(ck, ());
    }
    let mut row_keys: Vec<Vec<u32>> = row_set.into_keys().collect();
    row_keys.sort();
    let mut col_keys: Vec<Vec<u32>> = col_set.into_keys().collect();
    col_keys.sort();
    Ok(Prepared {
        row_items,
        col_items,
        row_keys,
        col_keys,
        accs,
    })
}

/// Lines of one axis. `top_subtotals`: the group line itself carries the subtotal
/// (compact/outline rows); otherwise a subtotal line follows each group (tabular, columns).
fn axis_lines(
    keys: &[Vec<u32>],
    depth: usize,
    subtotals: bool,
    top_subtotals: bool,
    grand: bool,
) -> Vec<Line> {
    let mut out = Vec::new();
    if depth == 0 {
        out.push(Line {
            key: Vec::new(),
            kind: LineKind::Item,
        });
        return out;
    }
    let mut prev: Option<&Vec<u32>> = None;
    for (i, k) in keys.iter().enumerate() {
        // Common prefix with the previous key: groups opening at levels below it.
        let common = prev.map_or(0, |p| p.iter().zip(k).take_while(|(a, b)| a == b).count());
        if top_subtotals {
            for level in common..depth.saturating_sub(1) {
                out.push(Line {
                    key: k.get(..=level).unwrap_or(&[]).to_vec(),
                    kind: LineKind::Subtotal,
                });
            }
        }
        out.push(Line {
            key: k.clone(),
            kind: LineKind::Item,
        });
        if !top_subtotals && subtotals {
            // Groups closing after this key.
            let next = keys.get(i + 1);
            let keep = next.map_or(0, |n| n.iter().zip(k).take_while(|(a, b)| a == b).count());
            for level in (keep..depth.saturating_sub(1)).rev() {
                out.push(Line {
                    key: k.get(..=level).unwrap_or(&[]).to_vec(),
                    kind: LineKind::Subtotal,
                });
            }
        }
        prev = Some(k);
    }
    if grand {
        out.push(Line {
            key: Vec::new(),
            kind: LineKind::Grand,
        });
    }
    out
}

fn item_value(items: &[FieldItems], level: usize, idx: u32) -> Value {
    items
        .get(level)
        .and_then(|f| f.items.get(idx as usize))
        .map(item_label)
        .unwrap_or(Value::Empty)
}

fn label_text(v: &Value) -> String {
    match v {
        Value::Empty => String::new(),
        other => other.to_display(),
    }
}

fn ratio(v: Value, d: Value) -> Value {
    match (v, d) {
        (Value::Number(a), Value::Number(b)) => {
            if b == 0.0 {
                Value::Error(ErrorKind::Div0)
            } else {
                crate::eval::num_value(a / b)
            }
        }
        (Value::Empty, _) => Value::Empty,
        (Value::Error(e), _) => Value::Error(e),
        (_, Value::Error(e)) => Value::Error(e),
        _ => Value::Error(ErrorKind::Div0),
    }
}

/// Text of the filter cell for a page field: `(All)`, the single shown item, or
/// `(Multiple Items)`.
fn page_label(p: &PivotTable, field: usize) -> Value {
    if p.hidden_items(field).is_empty() {
        return Value::text("(All)");
    }
    let shown = field_items(&p.cache, field, p.hidden_items(field)).items;
    match shown.as_slice() {
        [one] => item_label(one),
        _ => Value::text("(Multiple Items)"),
    }
}

/// Compute the output block of a pivot table from its cache.
pub fn layout(p: &PivotTable) -> Result<PivotOutput, String> {
    let pr = prepare(p)?;
    let nr = p.rows.len();
    let nc = p.cols.len();
    let nd = p.values.len();
    let dn = nd.max(1);
    let compact = p.layout == PivotLayout::Compact;
    let row_lines = axis_lines(
        &pr.row_keys,
        nr,
        p.subtotals,
        p.layout != PivotLayout::Tabular,
        p.grand_total_row && nr > 0,
    );
    let col_lines = axis_lines(
        &pr.col_keys,
        nc,
        p.subtotals,
        false,
        p.grand_total_col && nc > 0,
    );
    let no_labels = nr == 0 && nc == 0;
    let label_cols = if no_labels {
        0
    } else if compact {
        1
    } else {
        nr.max(1)
    };
    let header_rows = if nc > 0 { 1 + nc + usize::from(nd > 1) } else { 1 };
    let page_rows = if p.filters.is_empty() {
        0
    } else {
        p.filters.len() + 1
    };
    let rows = page_rows + header_rows + row_lines.len();
    let cols = (label_cols + col_lines.len().saturating_mul(dn)).max(if page_rows > 0 { 2 } else { 1 });
    if rows as u64 * cols as u64 > MAX_CELLS_PER_OP / 4
        || rows as u64 > u64::from(MAX_ROWS)
        || cols as u64 > u64::from(MAX_COLS)
    {
        return Err("The PivotTable is too large.".into());
    }
    let mut out = PivotOutput {
        rows,
        cols,
        cells: vec![OutCell::default(); rows * cols],
        page_rows,
        header_rows,
        label_cols,
    };
    // Report filters.
    for (i, f) in p.filters.iter().enumerate() {
        out.set(i, 0, Value::text(p.field_name(*f)), CellKind::Header);
        out.set(i, 1, page_label(p, *f), CellKind::Label(0));
    }
    let h0 = page_rows;
    let body0 = h0 + header_rows;
    // Column headers.
    if nc == 0 {
        if !no_labels {
            if compact {
                if nr > 0 {
                    out.set(h0, 0, Value::text("Row Labels"), CellKind::Header);
                }
            } else {
                for (l, f) in p.rows.iter().enumerate() {
                    out.set(h0, l, Value::text(p.field_name(*f)), CellKind::Header);
                }
            }
        }
        for d in 0..nd {
            out.set(h0, label_cols + d, Value::text(p.data_caption(d)), CellKind::Header);
        }
    } else {
        if nd == 1 {
            out.set(h0, 0, Value::text(p.data_caption(0)), CellKind::Header);
        }
        let corner = if compact {
            "Column Labels".to_string()
        } else {
            p.cols
                .first()
                .map(|f| p.field_name(*f).to_string())
                .unwrap_or_default()
        };
        out.set(h0, label_cols, Value::text(corner), CellKind::Header);
        let mut prev: Option<&Vec<u32>> = None;
        for (ci, line) in col_lines.iter().enumerate() {
            let c0 = label_cols + ci * dn;
            match line.kind {
                LineKind::Grand => {
                    if nd > 1 {
                        for d in 0..nd {
                            out.set(
                                h0 + 1 + nc,
                                c0 + d,
                                Value::text(format!("Total {}", p.data_caption(d))),
                                CellKind::TotalLabel,
                            );
                        }
                    } else {
                        out.set(h0 + 1, c0, Value::text("Grand Total"), CellKind::TotalLabel);
                    }
                }
                LineKind::Subtotal => {
                    let level = line.key.len().saturating_sub(1);
                    let name = label_text(&item_value(
                        &pr.col_items,
                        level,
                        line.key.last().copied().unwrap_or(0),
                    ));
                    if nd > 1 {
                        for d in 0..nd {
                            out.set(
                                h0 + 1 + level,
                                c0 + d,
                                Value::text(format!("{name} {}", p.data_caption(d))),
                                CellKind::TotalLabel,
                            );
                        }
                    } else {
                        out.set(
                            h0 + 1 + level,
                            c0,
                            Value::text(format!("{name} Total")),
                            CellKind::TotalLabel,
                        );
                    }
                }
                LineKind::Item => {
                    let common =
                        prev.map_or(0, |pk| pk.iter().zip(&line.key).take_while(|(a, b)| a == b).count());
                    for (level, idx) in line.key.iter().enumerate().skip(common) {
                        out.set(
                            h0 + 1 + level,
                            c0,
                            item_value(&pr.col_items, level, *idx),
                            CellKind::Label(0),
                        );
                    }
                    prev = Some(&line.key);
                    if nd > 1 {
                        for d in 0..nd {
                            out.set(
                                h0 + 1 + nc,
                                c0 + d,
                                Value::text(p.data_caption(d)),
                                CellKind::Header,
                            );
                        }
                    }
                }
            }
        }
        let last = h0 + header_rows - 1;
        if compact {
            if nr > 0 {
                out.set(last, 0, Value::text("Row Labels"), CellKind::Header);
            }
        } else {
            for (l, f) in p.rows.iter().enumerate() {
                out.set(last, l, Value::text(p.field_name(*f)), CellKind::Header);
            }
        }
    }
    // Body.
    let grand = pr.accs.get(&(Vec::new(), Vec::new()));
    let mut prev_row: Option<&Vec<u32>> = None;
    for (ri, rl) in row_lines.iter().enumerate() {
        let r = body0 + ri;
        let total_line = rl.kind != LineKind::Item;
        // Labels.
        match rl.kind {
            LineKind::Grand => out.set(r, 0, Value::text("Grand Total"), CellKind::TotalLabel),
            LineKind::Subtotal if p.layout == PivotLayout::Tabular => {
                let level = rl.key.len().saturating_sub(1);
                let name = label_text(&item_value(
                    &pr.row_items,
                    level,
                    rl.key.last().copied().unwrap_or(0),
                ));
                out.set(r, level, Value::text(format!("{name} Total")), CellKind::TotalLabel);
            }
            LineKind::Subtotal | LineKind::Item if nr == 0 => {
                if nd == 1 && nc > 0 {
                    out.set(r, 0, Value::text(p.data_caption(0)), CellKind::Header);
                }
            }
            LineKind::Subtotal | LineKind::Item => {
                let level = rl.key.len().saturating_sub(1);
                let v = item_value(&pr.row_items, level, rl.key.last().copied().unwrap_or(0));
                if compact {
                    out.set(r, 0, v, CellKind::Label(level.min(255) as u8));
                } else if p.layout == PivotLayout::Outline {
                    out.set(r, level, v, CellKind::Label(0));
                } else {
                    // Tabular: outer labels on the first row of their group only.
                    let common = prev_row
                        .map_or(0, |pk| pk.iter().zip(&rl.key).take_while(|(a, b)| a == b).count());
                    for (l, idx) in rl.key.iter().enumerate().skip(common) {
                        out.set(r, l, item_value(&pr.row_items, l, *idx), CellKind::Label(0));
                    }
                    prev_row = Some(&rl.key);
                }
            }
        }
        // Values: group lines without subtotals stay blank.
        let show_values = !(rl.kind == LineKind::Subtotal && !p.subtotals);
        if !show_values || nd == 0 {
            continue;
        }
        for (ci, cl) in col_lines.iter().enumerate() {
            let total = total_line || cl.kind != LineKind::Item;
            for (d, df) in p.values.iter().enumerate() {
                let c = label_cols + ci * dn + d;
                let raw = pr
                    .accs
                    .get(&(rl.key.clone(), cl.key.clone()))
                    .and_then(|a| a.get(d))
                    .map(|a| a.result(df.func));
                let Some(v) = raw else {
                    continue;
                };
                let denom = |k: &GroupKey| {
                    pr.accs
                        .get(k)
                        .and_then(|a| a.get(d))
                        .map_or(Value::Empty, |a| a.result(df.func))
                };
                let (v, pct) = match df.show_as {
                    ShowAs::Normal => (v, false),
                    ShowAs::PercentOfTotal => (
                        ratio(
                            v,
                            grand
                                .and_then(|a| a.get(d))
                                .map_or(Value::Empty, |a| a.result(df.func)),
                        ),
                        true,
                    ),
                    ShowAs::PercentOfRow => (ratio(v, denom(&(rl.key.clone(), Vec::new()))), true),
                    ShowAs::PercentOfCol => (ratio(v, denom(&(Vec::new(), cl.key.clone()))), true),
                };
                let kind = if total {
                    CellKind::TotalValue
                } else {
                    CellKind::Value
                };
                out.set_value(r, c, v, kind, pct, d);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// GETPIVOTDATA

fn same_item(a: &Value, b: &Value) -> bool {
    if item_key(a) == item_key(b) {
        return true;
    }
    match (a, b) {
        (Value::Number(n), Value::Text(t)) | (Value::Text(t), Value::Number(n)) => {
            t.trim().parse::<f64>().is_ok_and(|x| x == *n)
        }
        _ => false,
    }
}

/// The value GETPIVOTDATA returns for `data_field` (a data caption or source field name)
/// restricted to `pairs` of (field name, item); `#REF!` when the field or item is not in
/// the pivot or nothing matches.
pub fn query(p: &PivotTable, data_field: &str, pairs: &[(String, Value)]) -> Value {
    let want = data_field.trim();
    let d = (0..p.values.len())
        .find(|i| p.data_caption(*i).eq_ignore_ascii_case(want))
        .or_else(|| {
            p.values
                .iter()
                .position(|df| p.field_name(df.field).eq_ignore_ascii_case(want))
        });
    let Some(d) = d else {
        return Value::Error(ErrorKind::Ref);
    };
    let Some(df) = p.values.get(d) else {
        return Value::Error(ErrorKind::Ref);
    };
    // (field index, item, on rows?)
    let mut conds: Vec<(usize, Value, bool)> = Vec::new();
    for (name, item) in pairs {
        let Some(f) = p.cache.field_index(name) else {
            return Value::Error(ErrorKind::Ref);
        };
        let on_rows = p.rows.contains(&f);
        if !on_rows && !p.cols.contains(&f) {
            return Value::Error(ErrorKind::Ref);
        }
        let items = field_items(&p.cache, f, p.hidden_items(f));
        let Some(found) = items.items.iter().find(|v| same_item(v, item)) else {
            return Value::Error(ErrorKind::Ref);
        };
        conds.push((f, found.clone(), on_rows));
    }
    let hidden: Vec<(usize, std::collections::HashSet<ItemKey>)> = p
        .field_filters
        .iter()
        .map(|ff| (ff.field, ff.hidden.iter().map(item_key).collect()))
        .collect();
    let empty = Value::Empty;
    let (mut all, mut sel, mut row_only, mut col_only) =
        (Acc::default(), Acc::default(), Acc::default(), Acc::default());
    let mut matched = false;
    'rec: for r in &p.cache.records {
        for (f, h) in &hidden {
            if h.contains(&item_key(r.get(*f).unwrap_or(&empty))) {
                continue 'rec;
            }
        }
        let v = r.get(df.field).unwrap_or(&empty);
        all.add(v);
        let mut row_ok = true;
        let mut col_ok = true;
        for (f, item, on_rows) in &conds {
            if item_key(r.get(*f).unwrap_or(&empty)) != item_key(item) {
                if *on_rows {
                    row_ok = false;
                } else {
                    col_ok = false;
                }
            }
        }
        if row_ok {
            row_only.add(v);
        }
        if col_ok {
            col_only.add(v);
        }
        if row_ok && col_ok {
            matched = true;
            sel.add(v);
        }
    }
    if !matched {
        return Value::Error(ErrorKind::Ref);
    }
    let v = sel.result(df.func);
    match df.show_as {
        ShowAs::Normal => v,
        ShowAs::PercentOfTotal => ratio(v, all.result(df.func)),
        ShowAs::PercentOfRow => ratio(v, row_only.result(df.func)),
        ShowAs::PercentOfCol => ratio(v, col_only.result(df.func)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use sp_model::pivot::DataField;

    fn t(s: &str) -> Value {
        Value::text(s)
    }
    fn n(x: f64) -> Value {
        Value::Number(x)
    }

    fn sales() -> PivotTable {
        let block = vec![
            vec![t("Region"), t("Product"), t("Qtr"), t("Sales")],
            vec![t("East"), t("Pen"), t("Q1"), n(10.0)],
            vec![t("West"), t("Pen"), t("Q1"), n(20.0)],
            vec![t("East"), t("Ink"), t("Q2"), n(5.0)],
            vec![t("east"), t("Pen"), t("Q2"), n(7.0)],
            vec![t("West"), t("Ink"), t("Q2"), n(8.0)],
        ];
        PivotTable {
            name: "P".into(),
            cache: build_cache(&block).unwrap(),
            rows: vec![0],
            values: vec![DataField {
                field: 3,
                ..DataField::default()
            }],
            ..PivotTable::default()
        }
    }

    fn grid(o: &PivotOutput) -> Vec<Vec<String>> {
        (0..o.rows)
            .map(|r| {
                (0..o.cols)
                    .map(|c| label_text(&o.get(r, c).unwrap().value))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn simple_rows_sum() {
        let o = layout(&sales()).unwrap();
        assert_eq!(
            grid(&o),
            vec![
                vec!["Row Labels", "Sum of Sales"],
                vec!["East", "22"],
                vec!["West", "28"],
                vec!["Grand Total", "50"],
            ]
        );
        assert_eq!(o.get(3, 1).unwrap().kind, CellKind::TotalValue);
    }

    #[test]
    fn rows_and_cols_with_totals() {
        let mut p = sales();
        p.cols = vec![2];
        let o = layout(&p).unwrap();
        assert_eq!(
            grid(&o),
            vec![
                vec!["Sum of Sales", "Column Labels", "", ""],
                vec!["Row Labels", "Q1", "Q2", "Grand Total"],
                vec!["East", "10", "12", "22"],
                vec!["West", "20", "8", "28"],
                vec!["Grand Total", "30", "20", "50"],
            ]
        );
    }

    #[test]
    fn compact_subtotals_and_tabular() {
        let mut p = sales();
        p.rows = vec![0, 1];
        let o = layout(&p).unwrap();
        assert_eq!(
            grid(&o)
                .iter()
                .map(|r| r.join("|"))
                .collect::<Vec<_>>(),
            vec![
                "Row Labels|Sum of Sales",
                "East|22",
                "Ink|5",
                "Pen|17",
                "West|28",
                "Ink|8",
                "Pen|20",
                "Grand Total|50",
            ]
        );
        assert_eq!(o.get(2, 0).unwrap().kind, CellKind::Label(1));
        p.layout = PivotLayout::Tabular;
        let o = layout(&p).unwrap();
        assert_eq!(
            grid(&o)
                .iter()
                .map(|r| r.join("|"))
                .collect::<Vec<_>>(),
            vec![
                "Region|Product|Sum of Sales",
                "East|Ink|5",
                "|Pen|17",
                "East Total||22",
                "West|Ink|8",
                "|Pen|20",
                "West Total||28",
                "Grand Total||50",
            ]
        );
        p.subtotals = false;
        p.grand_total_row = false;
        let o = layout(&p).unwrap();
        assert_eq!(o.rows, 5);
    }

    #[test]
    fn aggregations() {
        let vals = [n(2.0), n(4.0), t("x"), Value::Empty, n(9.0)];
        let r = |f| aggregate(vals.iter(), f);
        assert_eq!(r(Aggregate::Sum), n(15.0));
        assert_eq!(r(Aggregate::Count), n(4.0));
        assert_eq!(r(Aggregate::CountNums), n(3.0));
        assert_eq!(r(Aggregate::Average), n(5.0));
        assert_eq!(r(Aggregate::Max), n(9.0));
        assert_eq!(r(Aggregate::Min), n(2.0));
        assert_eq!(r(Aggregate::Product), n(72.0));
        assert_eq!(r(Aggregate::Var), n(13.0));
        let sd = r(Aggregate::StdDev).as_number().unwrap();
        assert!((sd - 13f64.sqrt()).abs() < 1e-12);
        let vp = r(Aggregate::VarP).as_number().unwrap();
        assert!((vp - 26.0 / 3.0).abs() < 1e-12);
        assert_eq!(
            aggregate([t("a")].iter(), Aggregate::Average),
            Value::Error(ErrorKind::Div0)
        );
        assert_eq!(
            aggregate([n(1.0), Value::Error(ErrorKind::NA)].iter(), Aggregate::Sum),
            Value::Error(ErrorKind::NA)
        );
    }

    #[test]
    fn show_as_percent() {
        let mut p = sales();
        p.cols = vec![2];
        p.values[0].show_as = ShowAs::PercentOfTotal;
        let o = layout(&p).unwrap();
        assert_eq!(o.get(2, 1).unwrap().value, n(0.2));
        assert!(o.get(2, 1).unwrap().percent);
        assert_eq!(o.get(4, 3).unwrap().value, n(1.0));
        p.values[0].show_as = ShowAs::PercentOfRow;
        let o = layout(&p).unwrap();
        assert_eq!(o.get(3, 1).unwrap().value, n(20.0 / 28.0));
        p.values[0].show_as = ShowAs::PercentOfCol;
        let o = layout(&p).unwrap();
        assert_eq!(o.get(3, 2).unwrap().value, n(0.4));
    }

    #[test]
    fn multiple_data_fields_and_filters() {
        let mut p = sales();
        p.values.push(DataField {
            field: 3,
            func: Aggregate::Count,
            ..DataField::default()
        });
        p.filters = vec![1];
        p.field_filters = vec![sp_model::pivot::FieldFilter {
            field: 1,
            hidden: vec![t("ink")],
        }];
        let o = layout(&p).unwrap();
        let g = grid(&o);
        assert_eq!(g[0][..2], ["Product".to_string(), "Pen".to_string()]);
        assert_eq!(g[2], vec!["Row Labels", "Sum of Sales", "Count of Sales"]);
        assert_eq!(g[3], vec!["East", "17", "2"]);
        assert_eq!(g[5], vec!["Grand Total", "37", "3"]);
    }

    #[test]
    fn values_only_and_empty() {
        let mut p = sales();
        p.rows.clear();
        let o = layout(&p).unwrap();
        assert_eq!(grid(&o), vec![vec!["Sum of Sales"], vec!["50"]]);
        p.values.clear();
        let o = layout(&p).unwrap();
        assert_eq!(o.rows, 2);
        p.rows = vec![99];
        assert!(layout(&p).is_err());
    }

    #[test]
    fn get_pivot_data() {
        let mut p = sales();
        p.cols = vec![2];
        assert_eq!(query(&p, "Sales", &[]), n(50.0));
        assert_eq!(query(&p, "Sum of Sales", &[("Region".into(), t("EAST"))]), n(22.0));
        assert_eq!(
            query(
                &p,
                "sales",
                &[("Region".into(), t("West")), ("Qtr".into(), t("Q2"))]
            ),
            n(8.0)
        );
        assert_eq!(
            query(&p, "Sales", &[("Region".into(), t("North"))]),
            Value::Error(ErrorKind::Ref)
        );
        assert_eq!(
            query(&p, "Sales", &[("Product".into(), t("Pen"))]),
            Value::Error(ErrorKind::Ref)
        );
        assert_eq!(query(&p, "Nope", &[]), Value::Error(ErrorKind::Ref));
    }

    #[test]
    fn cache_headers() {
        assert!(build_cache(&[vec![t("a"), Value::Empty]]).is_err());
        let c = build_cache(&[vec![t("a"), t("A")], vec![n(1.0)]]).unwrap();
        assert_eq!(c.fields, vec!["a", "A2"]);
        assert_eq!(c.records[0].len(), 2);
        assert!(build_cache(&[]).is_err());
    }

    #[test]
    fn item_order() {
        let mut v = vec![t("b"), Value::Empty, n(3.0), t("A"), n(-1.0), Value::Bool(true)];
        v.sort_by(cmp_items);
        assert_eq!(
            v,
            vec![n(-1.0), n(3.0), t("A"), t("b"), Value::Bool(true), Value::Empty]
        );
    }
}
