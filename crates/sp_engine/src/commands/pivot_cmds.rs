//! Pivot tables, slicers and consolidation: `insert.pivot_table`, `data.refresh_all`,
//! `insert.slicer`, `data.consolidate`.
//!
//! A pivot's definition lives in `Sheet::analysis` (`sp_model::pivot`); a refresh snapshots
//! the source into the pivot cache, lays the report out (`sp_formula::pivot::layout`) and
//! writes it into the sheet as constants, all inside the calling transaction.

use crate::params::{bool_opt, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::addr::{col_to_letters, quote_sheet_name, split_sheet_prefix};
use sp_core::{CellAddr, CellRange, MAX_CELLS_PER_OP, MAX_COLS, MAX_ROWS, Value};
use sp_formula::pivot::{CellKind, all_items, build_cache, item_key, item_label, layout};
use sp_model::pivot::{
    Aggregate, DataField, FieldFilter, MAX_OBJECTS, MAX_RECORDS, PivotLayout, PivotTable, ShowAs,
    Slicer,
};
use sp_model::chart::Anchor;
use sp_model::{Cell, Style, StyleId, Workbook};

fn bad(m: impl Into<String>) -> CmdError {
    CmdError::Param(m.into())
}

fn failed(m: impl Into<String>) -> CmdError {
    CmdError::Failed(m.into())
}

// ---------------------------------------------------------------------------------------
// References

/// `$A$1` / `$A$1:$D$9` (single cells collapse).
pub fn abs_ref(r: CellRange) -> String {
    let a = |c: CellAddr| format!("${}${}", col_to_letters(c.col), c.row + 1);
    if r.is_single() {
        a(r.start)
    } else {
        format!("{}:{}", a(r.start), a(r.end))
    }
}

/// `Sheet1!$A$1:$D$9`.
pub fn qualified(wb: &Workbook, sheet: usize, r: CellRange) -> String {
    let name = wb.sheet(sheet).map_or("", |s| s.name.as_str());
    format!("{}!{}", quote_sheet_name(name), abs_ref(r))
}

/// Parse a sheet-local or qualified reference text (`$F$3`, `Data!$A$1:$C$9`); `home` is
/// the sheet used when the text has no prefix.
pub fn parse_ref_text(wb: &Workbook, home: usize, text: &str) -> Option<(usize, CellRange)> {
    let (sheet, body) = split_sheet_prefix(text.trim());
    let idx = match sheet {
        Some(n) => wb.sheet_index(&n)?,
        None => home,
    };
    wb.sheet(idx)?;
    CellRange::parse(&body.replace('$', "")).ok().map(|r| (idx, r))
}

/// Resolve a pivot source: a reference or a table name (its header + data rows).
fn resolve_source(wb: &Workbook, home: usize, text: &str) -> Result<(usize, CellRange), String> {
    if let Some(r) = parse_ref_text(wb, home, text) {
        return Ok(r);
    }
    let name = text.trim();
    if let Some((i, t)) = wb.find_table(name) {
        let mut r = t.range;
        if t.totals_row && r.end.row > r.start.row {
            r.end.row -= 1;
        }
        return Ok((i, r));
    }
    Err("The PivotTable source reference is not valid.".into())
}

/// The source block (header row first), trimmed to the sheet's used area.
fn read_block(wb: &Workbook, sheet: usize, r: CellRange) -> Result<Vec<Vec<Value>>, String> {
    let sh = wb
        .sheet(sheet)
        .ok_or_else(|| "The PivotTable source reference is not valid.".to_string())?;
    let used = sh
        .used_range()
        .unwrap_or_else(|| CellRange::single(CellAddr::default()));
    // Whole rows/columns stop at the used area; an explicit block is kept as given.
    let mut r = r;
    if r.end.row > used.end.row && r.start.row <= used.end.row {
        r.end.row = used.end.row.max(r.start.row);
    }
    if r.end.col > used.end.col && r.start.col <= used.end.col {
        r.end.col = used.end.col.max(r.start.col);
    }
    if r.rows() < 1 || r.cell_count() > MAX_CELLS_PER_OP {
        return Err("The PivotTable source range is too large.".into());
    }
    if r.rows() as usize > MAX_RECORDS.saturating_add(1) {
        return Err("The PivotTable source range has too many rows.".into());
    }
    let w = r.cols() as usize;
    let mut block: Vec<Vec<Value>> = (r.start.row..=r.end.row)
        .map(|_| vec![Value::Empty; w])
        .collect();
    for (a, c) in sh.cells_in(r) {
        let (i, j) = ((a.row - r.start.row) as usize, (a.col - r.start.col) as usize);
        if let Some(slot) = block.get_mut(i).and_then(|row| row.get_mut(j)) {
            *slot = c.value.clone();
        }
    }
    Ok(block)
}

// ---------------------------------------------------------------------------------------
// Refresh

fn style_for(
    wb: &mut Workbook,
    base: StyleId,
    kind: CellKind,
    percent: bool,
    num_fmt: &str,
) -> StyleId {
    let bold = matches!(kind, CellKind::Header | CellKind::TotalLabel | CellKind::TotalValue);
    let indent = match kind {
        CellKind::Label(n) => n.min(15),
        _ => 0,
    };
    if !bold && indent == 0 && !percent && num_fmt.is_empty() {
        return base;
    }
    wb.styles.derive(base, |st: &mut Style| {
        st.bold = st.bold || bold;
        if indent > 0 {
            st.indent = indent;
        }
        if !num_fmt.is_empty() {
            st.num_fmt = num_fmt.to_string();
        } else if percent {
            st.num_fmt = "0.00%".into();
        }
    })
}

/// Map field indexes of the old cache onto a refreshed one (by name); fields that vanished
/// from the source drop out of the definition.
fn remap(p: &mut PivotTable, new_fields: &[String]) -> Vec<Option<usize>> {
    let map: Vec<Option<usize>> = p
        .cache
        .fields
        .iter()
        .map(|f| new_fields.iter().position(|n| n.eq_ignore_ascii_case(f)))
        .collect();
    let m = |i: usize| map.get(i).copied().flatten();
    p.rows = p.rows.iter().filter_map(|i| m(*i)).collect();
    p.cols = p.cols.iter().filter_map(|i| m(*i)).collect();
    p.filters = p.filters.iter().filter_map(|i| m(*i)).collect();
    p.values = p
        .values
        .iter()
        .filter_map(|d| {
            m(d.field).map(|f| DataField {
                field: f,
                ..d.clone()
            })
        })
        .collect();
    p.field_filters = p
        .field_filters
        .iter()
        .filter_map(|ff| {
            m(ff.field).map(|f| FieldFilter {
                field: f,
                hidden: ff.hidden.clone(),
            })
        })
        .collect();
    map
}

/// Refresh pivot `idx` of `sheet`: re-read its source, recompute, rewrite its output cells.
/// The caller has touched `sheet` in its transaction. `overwrite`: write over non-empty
/// cells outside the previous output (otherwise refuse).
pub fn refresh_pivot(
    wb: &mut Workbook,
    sheet: usize,
    idx: usize,
    overwrite: bool,
) -> Result<CellRange, String> {
    let mut p = wb
        .sheet(sheet)
        .and_then(|s| s.analysis.pivots.get(idx))
        .cloned()
        .ok_or_else(|| "no such PivotTable".to_string())?;
    let (src_sheet, src) = resolve_source(wb, sheet, &p.source)?;
    let block = read_block(wb, src_sheet, src)?;
    let cache = build_cache(&block)?;
    let had_cache = !p.cache.fields.is_empty();
    let map = if had_cache {
        remap(&mut p, &cache.fields)
    } else {
        Vec::new()
    };
    p.cache = cache;
    let out = layout(&p)?;
    let (_, loc) = parse_ref_text(wb, sheet, &p.location)
        .ok_or_else(|| "The PivotTable location is not valid.".to_string())?;
    let at = loc.start;
    let end_row = u64::from(at.row) + out.rows.max(1) as u64 - 1;
    let end_col = u64::from(at.col) + out.cols.max(1) as u64 - 1;
    if end_row >= u64::from(MAX_ROWS) || end_col >= u64::from(MAX_COLS) {
        return Err("The PivotTable does not fit on the sheet.".into());
    }
    let area = CellRange::new(at, CellAddr::new(end_row as u32, end_col as u32));
    let old = parse_ref_text(wb, sheet, &p.output).map(|(_, r)| r);
    let sh = wb
        .sheet(sheet)
        .ok_or_else(|| "no such sheet".to_string())?;
    for (i, other) in sh.analysis.pivots.iter().enumerate() {
        if i == idx {
            continue;
        }
        let o = parse_ref_text(wb, sheet, &other.output)
            .or_else(|| parse_ref_text(wb, sheet, &other.location))
            .map(|(_, r)| r);
        if o.is_some_and(|o| o.intersect(&area).is_some()) {
            return Err("A PivotTable report cannot overlap another PivotTable report.".into());
        }
    }
    if sh.tables.iter().any(|t| t.range.intersect(&area).is_some()) {
        return Err("A PivotTable report cannot overlap a table.".into());
    }
    if !overwrite {
        let busy = sh
            .cells_in(area)
            .any(|(a, c)| !c.is_blank() && !old.is_some_and(|o| o.contains(a)));
        if busy {
            return Err(format!(
                "There's already data in {}. Pass \"overwrite\": true to replace it.",
                abs_ref(area)
            ));
        }
    }
    // Clear the previous output, then write the new one.
    let mut styled: Vec<(CellAddr, Cell)> = Vec::with_capacity(out.cells.len());
    for r in 0..out.rows {
        for c in 0..out.cols {
            let Some(oc) = out.get(r, c) else { continue };
            let a = CellAddr::new(at.row + r as u32, at.col + c as u32);
            if oc.value.is_empty() && oc.kind == CellKind::Blank {
                continue;
            }
            let fmt = oc
                .data
                .and_then(|d| p.values.get(d))
                .map_or("", |d| d.num_fmt.as_str());
            let style = style_for(wb, StyleId::DEFAULT, oc.kind, oc.percent, fmt);
            styled.push((
                a,
                Cell {
                    style,
                    ..Cell::constant(oc.value.clone())
                },
            ));
        }
    }
    let sh = wb
        .sheet_mut(sheet)
        .ok_or_else(|| "no such sheet".to_string())?;
    if let Some(o) = old {
        let addrs: Vec<CellAddr> = sh.cells_in(o).map(|(a, _)| a).collect();
        for a in addrs {
            sh.remove(a);
        }
    }
    let addrs: Vec<CellAddr> = sh.cells_in(area).map(|(a, _)| a).collect();
    for a in addrs {
        sh.remove(a);
    }
    for (a, c) in styled {
        sh.set(a, c);
    }
    p.output = abs_ref(area);
    let name = p.name.clone();
    if let Some(slot) = sh.analysis.pivots.get_mut(idx) {
        *slot = p;
    }
    // Slicers follow their field through the remap.
    if had_cache {
        for s in &mut sh.analysis.slicers {
            if s.pivot.eq_ignore_ascii_case(&name)
                && let Some(Some(f)) = map.get(s.field)
            {
                s.field = *f;
            }
        }
    }
    Ok(area)
}

// ---------------------------------------------------------------------------------------
// insert.pivot_table

fn field_of(fields: &[String], v: &Json) -> Result<usize, CmdError> {
    let name = match v {
        Json::String(s) => s.clone(),
        Json::Object(o) => o
            .get("field")
            .and_then(Json::as_str)
            .ok_or_else(|| bad("field entry needs a 'field' name"))?
            .to_string(),
        _ => return Err(bad("fields are given by name")),
    };
    fields
        .iter()
        .position(|f| f.eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| bad(format!("no field named '{name}' in the source")))
}

fn list<'a>(p: &'a Json, k: &str) -> Result<&'a [Json], CmdError> {
    match p.get(k) {
        None | Some(Json::Null) => Ok(&[]),
        Some(Json::Array(a)) if a.len() <= 256 => Ok(a.as_slice()),
        Some(Json::Array(_)) => Err(bad(format!("too many entries in '{k}'"))),
        Some(Json::String(_)) => Ok(std::slice::from_ref(p.get(k).unwrap_or(&Json::Null))),
        Some(_) => Err(bad(format!("'{k}' must be a list"))),
    }
}

/// A JSON item → cell value (strings, numbers, booleans, null = blank).
fn json_value(v: &Json) -> Value {
    match v {
        Json::Number(n) => n.as_f64().map_or(Value::Empty, Value::Number),
        Json::Bool(b) => Value::Bool(*b),
        Json::String(s) if s == "(blank)" => Value::Empty,
        Json::String(s) => Value::text(s.as_str()),
        _ => Value::Empty,
    }
}

/// Hidden items so that only `shown` remain visible (matched by value or by label text).
fn hidden_except(all: &[Value], shown: &[Json]) -> Vec<Value> {
    let wanted: Vec<Value> = shown.iter().map(json_value).collect();
    all.iter()
        .filter(|item| {
            !wanted.iter().any(|w| {
                item_key(w) == item_key(item)
                    || item_label(item)
                        .to_display()
                        .eq_ignore_ascii_case(&w.to_display())
            })
        })
        .cloned()
        .collect()
}

fn set_hidden(p: &mut PivotTable, field: usize, hidden: Vec<Value>) {
    p.field_filters.retain(|f| f.field != field);
    if !hidden.is_empty() {
        p.field_filters.push(FieldFilter { field, hidden });
    }
}

fn unique_pivot_name(wb: &Workbook, base: &str) -> String {
    let taken = |n: &str| {
        wb.sheets
            .iter()
            .any(|s| s.analysis.pivots.iter().any(|p| p.name.eq_ignore_ascii_case(n)))
    };
    (1u32..=100_000)
        .map(|n| format!("{base}{n}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| format!("{base}_"))
}

/// Default data-field function: Sum for numeric fields, Count otherwise.
fn default_func(p: &PivotTable, field: usize) -> Aggregate {
    let numeric = p.cache.records.iter().all(|r| {
        matches!(
            r.get(field),
            None | Some(Value::Empty) | Some(Value::Number(_))
        )
    });
    if numeric {
        Aggregate::Sum
    } else {
        Aggregate::Count
    }
}

/// Fill the definition of `pv` (whose cache is built) from params.
fn define(pv: &mut PivotTable, p: &Json) -> Result<(), CmdError> {
    let fields = pv.cache.fields.clone();
    let mut used: Vec<usize> = Vec::new();
    let axis = |k: &str, used: &mut Vec<usize>| -> Result<Vec<usize>, CmdError> {
        let mut v = Vec::new();
        for e in list(p, k)? {
            let f = field_of(&fields, e)?;
            if used.contains(&f) {
                return Err(bad(format!(
                    "field '{}' is already used in another area",
                    fields.get(f).map_or("", String::as_str)
                )));
            }
            used.push(f);
            v.push(f);
        }
        Ok(v)
    };
    pv.rows = axis("rows", &mut used)?;
    pv.cols = axis("cols", &mut used)?;
    if pv.cols.is_empty() {
        pv.cols = axis("columns", &mut used)?;
    }
    pv.filters = axis("filters", &mut used)?;
    // Item selections on filter entries (`{"field": "Region", "items": ["East"]}`).
    for e in list(p, "filters")? {
        if let Some(items) = e.get("items").and_then(Json::as_array) {
            let f = field_of(&fields, e)?;
            let all = all_items(&pv.cache, f);
            set_hidden(pv, f, hidden_except(&all, items));
        }
    }
    pv.values.clear();
    for e in list(p, "values")? {
        let f = field_of(&fields, e)?;
        let func = match e.get("func").or_else(|| e.get("function")).and_then(Json::as_str) {
            Some(n) => Aggregate::from_name(n)
                .ok_or_else(|| bad(format!("unknown summary function '{n}'")))?,
            None => default_func(pv, f),
        };
        let show_as = match e.get("show_as").and_then(Json::as_str) {
            Some(n) => ShowAs::from_name(n)
                .ok_or_else(|| bad(format!("unknown show-values-as '{n}'")))?,
            None => ShowAs::Normal,
        };
        let name = e
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or("")
            .chars()
            .take(255)
            .collect();
        let num_fmt = e
            .get("num_fmt")
            .and_then(Json::as_str)
            .unwrap_or("")
            .chars()
            .take(255)
            .collect();
        pv.values.push(DataField {
            field: f,
            func,
            show_as,
            name,
            num_fmt,
        });
    }
    if let Some(l) = str_opt(p, "layout") {
        pv.layout = PivotLayout::from_name(l).ok_or_else(|| bad(format!("unknown layout '{l}'")))?;
    }
    if let Some(b) = bool_opt(p, "grand_totals") {
        pv.grand_total_row = b;
        pv.grand_total_col = b;
    }
    if let Some(b) = bool_opt(p, "grand_total_row") {
        pv.grand_total_row = b;
    }
    if let Some(b) = bool_opt(p, "grand_total_col") {
        pv.grand_total_col = b;
    }
    if let Some(b) = bool_opt(p, "subtotals") {
        pv.subtotals = b;
    }
    if let Some(st) = str_opt(p, "style") {
        if st.len() > 255 {
            return Err(bad("style name too long"));
        }
        pv.style = st.to_string();
    }
    Ok(())
}

fn insert_pivot_table(s: &mut Session, p: &Json) -> CmdResult {
    let home = s.sheet_index();
    // Source: explicit, else the table at the active cell, else the current region.
    let source_text = match str_opt(p, "source") {
        Some(t) => {
            let (i, r) = resolve_source(&s.wb, home, t).map_err(bad)?;
            if s.wb.find_table(t.trim()).is_some() && parse_ref_text(&s.wb, home, t).is_none() {
                t.trim().to_string()
            } else {
                qualified(&s.wb, i, r)
            }
        }
        None => {
            let at = s.sel.active;
            match s.wb.sheet(home).and_then(|sh| sh.table_at(at)) {
                Some(t) => t.name.clone(),
                None => {
                    let r = if s.sel.range().is_single() {
                        super::nav_cmds::current_region(s, at)
                    } else {
                        s.sel.range()
                    };
                    qualified(&s.wb, home, r)
                }
            }
        }
    };
    let (src_sheet, src) = resolve_source(&s.wb, home, &source_text).map_err(bad)?;
    let block = read_block(&s.wb, src_sheet, src).map_err(failed)?;
    if block.len() < 2 {
        return Err(failed(
            "The PivotTable source needs a header row and at least one row of data.",
        ));
    }
    let cache = build_cache(&block).map_err(failed)?;
    let name = match str_opt(p, "name") {
        Some(n) if n.trim().is_empty() || n.len() > 255 => {
            return Err(bad("PivotTable name must be 1-255 characters"));
        }
        Some(n) => {
            let taken = s.wb.sheets.iter().any(|sh| {
                sh.analysis
                    .pivots
                    .iter()
                    .any(|x| x.name.eq_ignore_ascii_case(n.trim()))
            });
            if taken {
                return Err(failed(format!("A PivotTable named '{n}' already exists.")));
            }
            n.trim().to_string()
        }
        None => unique_pivot_name(&s.wb, "PivotTable"),
    };
    let mut pv = PivotTable {
        name: name.clone(),
        source: source_text,
        cache,
        ..PivotTable::default()
    };
    define(&mut pv, p)?;
    let records = pv.cache.records.len();
    // Destination: an explicit cell, else a new sheet.
    let dest = match str_opt(p, "dest").or_else(|| str_opt(p, "location")) {
        Some(t) => Some(
            parse_ref_text(&s.wb, home, t)
                .ok_or_else(|| bad(format!("not a reference: '{t}'")))?,
        ),
        None => None,
    };
    if let Some((ds, _)) = dest
        && s.wb.sheet(ds).is_some_and(|sh| sh.analysis.pivots.len() >= MAX_OBJECTS)
    {
        return Err(failed("too many PivotTables on this sheet"));
    }
    let overwrite = bool_opt(p, "overwrite").unwrap_or(false);
    let new_sheet_name = s.wb.next_sheet_name();
    let filters = !pv.filters.is_empty();
    let (sheet, area) = s.transact("Insert PivotTable", move |tx, _| {
        let (sheet, at) = match dest {
            Some((ds, r)) => {
                tx.touch_sheet(ds);
                (ds, r.start)
            }
            None => {
                tx.touch_book();
                let at = home.min(tx.wb.sheets.len());
                tx.wb.sheets.insert(at, sp_model::Sheet::new(new_sheet_name));
                for n in &mut tx.wb.names {
                    if let Some(sc) = n.scope.as_mut()
                        && *sc >= at
                    {
                        *sc += 1;
                    }
                }
                tx.wb.active = at;
                // The report starts at A3 (filters, when any, from A1).
                (at, CellAddr::new(if filters { 0 } else { 2 }, 0))
            }
        };
        pv.location = abs_ref(CellRange::single(at));
        // Build from the cache already read (the definition is validated against it).
        pv.cache = sp_model::pivot::PivotCache::default();
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| failed("no such sheet"))?;
        sh.analysis.pivots.push(pv);
        let idx = sh.analysis.pivots.len() - 1;
        let area = refresh_pivot(tx.wb, sheet, idx, overwrite).map_err(failed)?;
        Ok((sheet, area))
    })?;
    if sheet == s.sheet_index() {
        s.select(area.start);
    }
    let sheet_name = s.wb.sheet(sheet).map_or(String::new(), |x| x.name.clone());
    Ok(json!({
        "name": name,
        "sheet": sheet_name,
        "range": area.to_a1(),
        "records": records,
    }))
}

// ---------------------------------------------------------------------------------------
// data.refresh_all

fn refresh_all(s: &mut Session, _p: &Json) -> CmdResult {
    let targets: Vec<(usize, usize)> = s
        .wb
        .sheets
        .iter()
        .enumerate()
        .flat_map(|(i, sh)| (0..sh.analysis.pivots.len()).map(move |j| (i, j)))
        .collect();
    if targets.is_empty() {
        s.recalc();
        return Ok(json!({"refreshed": 0, "errors": []}));
    }
    let (n, errors) = s.transact("Refresh All", |tx, _| {
        let mut n = 0usize;
        let mut errors: Vec<Json> = Vec::new();
        for (sheet, idx) in &targets {
            tx.touch_sheet(*sheet);
            let name = tx
                .wb
                .sheet(*sheet)
                .and_then(|sh| sh.analysis.pivots.get(*idx))
                .map_or(String::new(), |p| p.name.clone());
            match refresh_pivot(tx.wb, *sheet, *idx, true) {
                Ok(_) => n += 1,
                Err(e) => errors.push(json!({"pivot": name, "error": e})),
            }
        }
        if n == 0 {
            let msg = errors
                .first()
                .and_then(|e| e.get("error"))
                .and_then(Json::as_str)
                .unwrap_or("nothing to refresh")
                .to_string();
            return Err(failed(msg));
        }
        Ok((n, errors))
    })?;
    // Data tables and volatile cells follow.
    s.recalc();
    Ok(json!({"refreshed": n, "errors": errors}))
}

// ---------------------------------------------------------------------------------------
// insert.slicer

/// (sheet, pivot index) named by `pivot`, else the pivot containing the active cell, else
/// the only pivot of the active sheet.
fn target_pivot(s: &Session, p: &Json) -> Result<(usize, usize), CmdError> {
    if let Some(n) = str_opt(p, "pivot") {
        for (i, sh) in s.wb.sheets.iter().enumerate() {
            if let Some(j) = sh
                .analysis
                .pivots
                .iter()
                .position(|x| x.name.eq_ignore_ascii_case(n.trim()))
            {
                return Ok((i, j));
            }
        }
        return Err(bad(format!("no PivotTable named '{n}'")));
    }
    let home = s.sheet_index();
    let sh = s
        .wb
        .sheet(home)
        .ok_or_else(|| failed("no active sheet"))?;
    let at = s.sel.active;
    let hit = sh.analysis.pivots.iter().position(|x| {
        parse_ref_text(&s.wb, home, &x.output).is_some_and(|(_, r)| r.contains(at))
    });
    match (hit, sh.analysis.pivots.len()) {
        (Some(j), _) => Ok((home, j)),
        (None, 1) => Ok((home, 0)),
        _ => Err(failed("Select a cell in a PivotTable (or pass 'pivot') first.")),
    }
}

fn slicer_items(pv: &PivotTable, field: usize) -> Json {
    let hidden: Vec<_> = pv.hidden_items(field).iter().map(item_key).collect();
    let items: Vec<Json> = all_items(&pv.cache, field)
        .iter()
        .map(|v| {
            json!({
                "item": item_label(v).to_display(),
                "selected": !hidden.contains(&item_key(v)),
            })
        })
        .collect();
    Json::Array(items)
}

fn insert_slicer(s: &mut Session, p: &Json) -> CmdResult {
    // An existing slicer by name: change its selection or delete it.
    let existing = str_opt(p, "name").and_then(|n| {
        s.wb.sheets.iter().enumerate().find_map(|(i, sh)| {
            sh.analysis
                .slicers
                .iter()
                .position(|x| x.name.eq_ignore_ascii_case(n.trim()))
                .map(|j| (i, j))
        })
    });
    let (sheet, slicer) = match existing {
        Some((i, j)) => {
            let sl = s
                .wb
                .sheet(i)
                .and_then(|sh| sh.analysis.slicers.get(j))
                .cloned()
                .ok_or_else(|| failed("no such slicer"))?;
            if bool_opt(p, "delete").unwrap_or(false) {
                let name = sl.name.clone();
                s.transact("Delete Slicer", |tx, _| {
                    tx.touch_sheet(i);
                    if let Some(sh) = tx.wb.sheet_mut(i) {
                        sh.analysis.slicers.retain(|x| !x.name.eq_ignore_ascii_case(&name));
                    }
                    Ok(())
                })?;
                return Ok(json!({"deleted": sl.name}));
            }
            (i, sl)
        }
        None => {
            let (ps, pj) = target_pivot(s, p)?;
            let pv = s
                .wb
                .sheet(ps)
                .and_then(|sh| sh.analysis.pivots.get(pj))
                .ok_or_else(|| failed("no such PivotTable"))?;
            let fv = p.get("field").ok_or_else(|| bad("missing string 'field'"))?;
            let field = field_of(&pv.cache.fields, fv)?;
            let fname = pv.field_name(field).to_string();
            if s.wb.sheet(ps).is_some_and(|sh| sh.analysis.slicers.len() >= MAX_OBJECTS) {
                return Err(failed("too many slicers on this sheet"));
            }
            let base: String = format!("Slicer_{}", fname.replace(|c: char| !c.is_alphanumeric(), "_"))
                .chars()
                .take(200)
                .collect();
            let taken = |n: &str| {
                s.wb.sheets
                    .iter()
                    .any(|sh| sh.analysis.slicers.iter().any(|x| x.name.eq_ignore_ascii_case(n)))
            };
            let name = match str_opt(p, "name") {
                Some(n) if !n.trim().is_empty() && n.len() <= 255 => n.trim().to_string(),
                Some(_) => return Err(bad("slicer name must be 1-255 characters")),
                None if !taken(&base) => base.clone(),
                None => (1u32..=100_000)
                    .map(|k| format!("{base}{k}"))
                    .find(|n| !taken(n))
                    .unwrap_or_else(|| format!("{base}_")),
            };
            let caption = str_opt(p, "caption")
                .map_or_else(|| fname.clone(), |c| c.chars().take(255).collect());
            let stacked = s
                .wb
                .sheet(ps)
                .map_or(0, |sh| sh.analysis.slicers.len());
            let anchor = default_slicer_anchor(&s.wb, ps, pv, stacked);
            (
                ps,
                Slicer {
                    name,
                    caption,
                    pivot: pv.name.clone(),
                    field,
                    anchor,
                    columns: 1,
                    ..Slicer::default()
                },
            )
        }
    };
    let anchor = anchor_param(s, p)?;
    let style = match str_opt(p, "style") {
        Some(st) if st.len() > 255 => return Err(bad("style name too long")),
        Some(st) => Some(st.to_string()),
        None => None,
    };
    let columns = match crate::params::u32_opt(p, "columns")? {
        Some(c) if (1..=20).contains(&c) => Some(c),
        Some(_) => return Err(bad("'columns' must be 1-20")),
        None => None,
    };
    let pj = s
        .wb
        .sheet(sheet)
        .and_then(|sh| {
            sh.analysis
                .pivots
                .iter()
                .position(|x| x.name.eq_ignore_ascii_case(&slicer.pivot))
        })
        .ok_or_else(|| failed(format!("the slicer's PivotTable '{}' no longer exists", slicer.pivot)))?;
    let select = match p.get("select").or_else(|| p.get("items")) {
        None | Some(Json::Null) => None,
        Some(Json::Array(a)) if a.len() <= 100_000 => Some(a.clone()),
        Some(Json::String(t)) => Some(vec![Json::String(t.clone())]),
        Some(_) => return Err(bad("'select' must be a list of items")),
    };
    let clear = bool_opt(p, "clear").unwrap_or(false);
    let is_new = existing.is_none();
    let mut sl = slicer.clone();
    if let Some(a) = anchor {
        sl.anchor = a;
    }
    if let Some(st) = style {
        sl.style = st;
    }
    if let Some(c) = columns {
        sl.columns = c;
    }
    let restyled = sl != slicer;
    let label = if is_new {
        "Insert Slicer"
    } else if anchor.is_some() && select.is_none() && !clear {
        "Move Slicer"
    } else {
        "Slicer"
    };
    if !is_new && !restyled && select.is_none() && !clear {
        // Nothing to change: report the slicer.
        let pv = s
            .wb
            .sheet(sheet)
            .and_then(|sh| sh.analysis.pivots.get(pj))
            .ok_or_else(|| failed("no such PivotTable"))?;
        return Ok(slicer_json(&slicer, pv));
    }
    let slicer = sl.clone();
    s.transact(label, move |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| failed("no such sheet"))?;
        if is_new {
            sh.analysis.slicers.push(sl.clone());
        } else if let Some(x) = sh
            .analysis
            .slicers
            .iter_mut()
            .find(|x| x.name.eq_ignore_ascii_case(&sl.name))
        {
            *x = sl.clone();
        }
        let pv = sh
            .analysis
            .pivots
            .get_mut(pj)
            .ok_or_else(|| failed("no such PivotTable"))?;
        if clear {
            set_hidden(pv, sl.field, Vec::new());
        } else if let Some(items) = &select {
            let all = all_items(&pv.cache, sl.field);
            let hidden = hidden_except(&all, items);
            if hidden.len() == all.len() && !all.is_empty() {
                return Err(bad("a slicer must leave at least one item selected"));
            }
            set_hidden(pv, sl.field, hidden);
        }
        if clear || select.is_some() {
            refresh_pivot(tx.wb, sheet, pj, true).map_err(failed)?;
        }
        Ok(())
    })?;
    let pv = s
        .wb
        .sheet(sheet)
        .and_then(|sh| sh.analysis.pivots.get(pj))
        .ok_or_else(|| failed("no such PivotTable"))?;
    Ok(slicer_json(&slicer, pv))
}

fn slicer_json(slicer: &Slicer, pv: &PivotTable) -> Json {
    json!({
        "name": slicer.name,
        "caption": slicer.caption,
        "pivot": slicer.pivot,
        "field": pv.field_name(slicer.field),
        "anchor": CellRange::new(slicer.anchor.from, slicer.anchor.to).to_a1(),
        "items": slicer_items(pv, slicer.field),
    })
}

/// A slicer next to its pivot: to the right of the output, stacked after `n` others.
fn default_slicer_anchor(wb: &Workbook, sheet: usize, pv: &PivotTable, n: usize) -> Anchor {
    let out = parse_ref_text(wb, sheet, &pv.output)
        .or_else(|| parse_ref_text(wb, sheet, &pv.location))
        .map_or(CellRange::single(CellAddr::default()), |(_, r)| r);
    let items = all_items(&pv.cache, 0).len().clamp(2, 10) as u32;
    let col = out
        .end
        .col
        .saturating_add(2)
        .saturating_add(3 * (n.min(100) as u32))
        .min(MAX_COLS - 4);
    let row = out.start.row.min(MAX_ROWS - 14);
    Anchor {
        from: CellAddr::new(row, col),
        from_off: (0.0, 0.0),
        to: CellAddr::new(row + items + 3, col + 2),
        to_off: (0.0, 0.0),
    }
}

/// `"anchor": "H2:J12"` → the slicer covers those cells.
fn anchor_param(s: &Session, p: &Json) -> Result<Option<Anchor>, CmdError> {
    let Some(t) = str_opt(p, "anchor") else {
        if p.get("anchor").is_some_and(|v| !v.is_null()) {
            return Err(bad("'anchor' must be a range like \"H2:J12\""));
        }
        return Ok(None);
    };
    let (_, r) = crate::params::parse_ref(s, t)?;
    if r.cell_count() > u64::from(MAX_ROWS) * 64 {
        return Err(bad("anchor range is too large"));
    }
    Ok(Some(Anchor {
        from: r.start,
        from_off: (0.0, 0.0),
        to: CellAddr::new(
            r.end.row.saturating_add(1).min(MAX_ROWS - 1),
            r.end.col.saturating_add(1).min(MAX_COLS - 1),
        ),
        to_off: (0.0, 0.0),
    }))
}

/// One slicer button: label, selected (not filtered out), and whether any record would
/// still show with it given the pivot's other filters.
#[derive(Debug, Clone, PartialEq)]
pub struct SlicerItem {
    pub label: String,
    pub value: Value,
    pub selected: bool,
    pub has_data: bool,
}

/// Most cache records scanned for the "no data" state (larger caches show every item as
/// having data).
const MAX_STATE_RECORDS: usize = 200_000;

/// The buttons of a slicer on `field` of `pv`.
pub fn slicer_state(pv: &PivotTable, field: usize) -> Vec<SlicerItem> {
    let hidden: Vec<_> = pv.hidden_items(field).iter().map(item_key).collect();
    let items = all_items(&pv.cache, field);
    let mut with_data: Option<std::collections::HashSet<sp_formula::pivot::ItemKey>> = None;
    if pv.cache.records.len() <= MAX_STATE_RECORDS {
        let others: Vec<(usize, Vec<sp_formula::pivot::ItemKey>)> = pv
            .field_filters
            .iter()
            .filter(|f| f.field != field && !f.hidden.is_empty())
            .map(|f| (f.field, f.hidden.iter().map(item_key).collect()))
            .collect();
        let mut set = std::collections::HashSet::new();
        for r in &pv.cache.records {
            let shown = others.iter().all(|(f, h)| {
                let k = item_key(r.get(*f).unwrap_or(&Value::Empty));
                !h.contains(&k)
            });
            if shown {
                set.insert(item_key(r.get(field).unwrap_or(&Value::Empty)));
            }
        }
        with_data = Some(set);
    }
    items
        .into_iter()
        .map(|v| {
            let k = item_key(&v);
            SlicerItem {
                label: item_label(&v).to_display(),
                selected: !hidden.contains(&k),
                has_data: with_data.as_ref().is_none_or(|s| s.contains(&k)),
                value: v,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// pivot.field (PivotTable Fields pane, pivot context menu)

/// Pivot areas a field can sit in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    Filters,
    Cols,
    Rows,
    Values,
}

impl Area {
    pub const ALL: [Area; 4] = [Area::Filters, Area::Cols, Area::Rows, Area::Values];

    pub fn name(self) -> &'static str {
        match self {
            Area::Filters => "filters",
            Area::Cols => "cols",
            Area::Rows => "rows",
            Area::Values => "values",
        }
    }

    pub fn from_name(s: &str) -> Option<Area> {
        Some(match s.to_ascii_lowercase().as_str() {
            "filters" | "filter" | "page" => Area::Filters,
            "cols" | "columns" | "col" | "column" => Area::Cols,
            "rows" | "row" => Area::Rows,
            "values" | "value" | "data" => Area::Values,
            _ => return None,
        })
    }
}

fn axis_mut(pv: &mut PivotTable, a: Area) -> Option<&mut Vec<usize>> {
    match a {
        Area::Filters => Some(&mut pv.filters),
        Area::Cols => Some(&mut pv.cols),
        Area::Rows => Some(&mut pv.rows),
        Area::Values => None,
    }
}

/// Whether every non-blank record value of `field` is a number.
pub fn field_is_numeric(pv: &PivotTable, field: usize) -> bool {
    let mut any = false;
    for r in &pv.cache.records {
        match r.get(field) {
            Some(Value::Number(_)) => any = true,
            None | Some(Value::Empty) => {}
            Some(_) => return false,
        }
    }
    any
}

/// The pivot (sheet, index) whose output contains `at` on `sheet`.
pub fn pivot_at(wb: &Workbook, sheet: usize, at: CellAddr) -> Option<usize> {
    let sh = wb.sheet(sheet)?;
    sh.analysis.pivots.iter().position(|x| {
        parse_ref_text(wb, sheet, &x.output)
            .or_else(|| parse_ref_text(wb, sheet, &x.location))
            .is_some_and(|(_, r)| r.contains(at))
    })
}

/// The pivot's definition as the Fields pane shows it.
pub fn pivot_info(pv: &PivotTable) -> Json {
    let names = |v: &[usize]| -> Vec<String> { v.iter().map(|f| pv.field_name(*f).to_string()).collect() };
    let fields: Vec<Json> = pv
        .cache
        .fields
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let mut areas = Vec::new();
            for a in Area::ALL {
                let used = match a {
                    Area::Filters => pv.filters.contains(&i),
                    Area::Cols => pv.cols.contains(&i),
                    Area::Rows => pv.rows.contains(&i),
                    Area::Values => pv.values.iter().any(|d| d.field == i),
                };
                if used {
                    areas.push(a.name());
                }
            }
            json!({
                "name": n,
                "areas": areas,
                "numeric": field_is_numeric(pv, i),
                "filtered": !pv.hidden_items(i).is_empty(),
            })
        })
        .collect();
    let values: Vec<Json> = pv
        .values
        .iter()
        .enumerate()
        .map(|(i, d)| {
            json!({
                "field": pv.field_name(d.field),
                "caption": pv.data_caption(i),
                "func": d.func.name(),
                "show_as": d.show_as.name(),
                "num_fmt": d.num_fmt,
            })
        })
        .collect();
    json!({
        "name": pv.name,
        "source": pv.source,
        "range": pv.output,
        "fields": fields,
        "filters": names(&pv.filters),
        "cols": names(&pv.cols),
        "rows": names(&pv.rows),
        "values": values,
        "layout": pv.layout.name(),
    })
}

/// Apply one Fields-pane edit to `pv`. Returns whether anything changed.
fn edit_fields(pv: &mut PivotTable, p: &Json) -> Result<bool, CmdError> {
    let area = match str_opt(p, "area") {
        None => None,
        Some("none") | Some("remove") => Some(None),
        Some(a) => Some(Some(Area::from_name(a).ok_or_else(|| bad(format!("unknown area '{a}'")))?)),
    };
    let position = crate::params::u32_opt(p, "position")?.map(|n| n as usize);
    let before = pv.clone();
    // A data field by index: settings, reorder, move out, remove.
    if let Some(vi) = crate::params::u32_opt(p, "value")? {
        let vi = vi as usize;
        if vi >= pv.values.len() {
            return Err(bad(format!("no value field {vi}")));
        }
        if bool_opt(p, "remove").unwrap_or(false) || area == Some(None) {
            pv.values.remove(vi);
            return Ok(true);
        }
        if let Some(d) = pv.values.get_mut(vi) {
            if let Some(n) = str_opt(p, "func").or_else(|| str_opt(p, "function")) {
                d.func = Aggregate::from_name(n).ok_or_else(|| bad(format!("unknown summary function '{n}'")))?;
            }
            if let Some(n) = str_opt(p, "show_as") {
                d.show_as = ShowAs::from_name(n).ok_or_else(|| bad(format!("unknown show-values-as '{n}'")))?;
            }
            if let Some(f) = str_opt(p, "num_fmt") {
                if f.len() > 255 {
                    return Err(bad("number format too long"));
                }
                d.num_fmt = if f.eq_ignore_ascii_case("general") { String::new() } else { f.to_string() };
            }
            if let Some(n) = str_opt(p, "name") {
                d.name = n.chars().take(255).collect();
            }
        }
        match area {
            Some(Some(Area::Values)) => {
                let d = pv.values.remove(vi);
                let at = position.unwrap_or(pv.values.len()).min(pv.values.len());
                pv.values.insert(at, d);
            }
            Some(Some(a)) => {
                let d = pv.values.remove(vi);
                place(pv, d.field, a, position);
            }
            _ => {}
        }
        return Ok(*pv != before);
    }
    let Some(fv) = p.get("field") else {
        return Ok(false);
    };
    let field = field_of(&pv.cache.fields, fv)?;
    if let Some(check) = bool_opt(p, "check") {
        let used = pv.rows.contains(&field)
            || pv.cols.contains(&field)
            || pv.filters.contains(&field)
            || pv.values.iter().any(|d| d.field == field);
        if check && !used {
            let a = if field_is_numeric(pv, field) { Area::Values } else { Area::Rows };
            place(pv, field, a, None);
        } else if !check {
            remove_field(pv, field);
        }
        return Ok(*pv != before);
    }
    match area {
        Some(None) => remove_field(pv, field),
        Some(Some(a)) => place(pv, field, a, position),
        None if bool_opt(p, "remove").unwrap_or(false) => remove_field(pv, field),
        None => {}
    }
    Ok(*pv != before)
}

fn remove_field(pv: &mut PivotTable, field: usize) {
    pv.rows.retain(|f| *f != field);
    pv.cols.retain(|f| *f != field);
    pv.filters.retain(|f| *f != field);
    pv.values.retain(|d| d.field != field);
}

/// Put `field` into area `a` at `position` (default: the end). Row/column/filter fields
/// are exclusive (moving takes it out of the other axes); a value field is added.
fn place(pv: &mut PivotTable, field: usize, a: Area, position: Option<usize>) {
    if a == Area::Values {
        let func = default_func(pv, field);
        let at = position.unwrap_or(pv.values.len()).min(pv.values.len());
        pv.values.insert(
            at,
            DataField {
                field,
                func,
                ..DataField::default()
            },
        );
        return;
    }
    for other in [Area::Filters, Area::Cols, Area::Rows] {
        if let Some(v) = axis_mut(pv, other) {
            v.retain(|f| *f != field);
        }
    }
    if let Some(v) = axis_mut(pv, a) {
        let at = position.unwrap_or(v.len()).min(v.len());
        v.insert(at, field);
    }
}

fn pivot_field(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, idx) = target_pivot(s, p)?;
    let mut pv = s
        .wb
        .sheet(sheet)
        .and_then(|sh| sh.analysis.pivots.get(idx))
        .cloned()
        .ok_or_else(|| failed("no such PivotTable"))?;
    let mut changed = edit_fields(&mut pv, p)?;
    if let Some(l) = str_opt(p, "layout") {
        pv.layout = PivotLayout::from_name(l).ok_or_else(|| bad(format!("unknown layout '{l}'")))?;
        changed = true;
    }
    if let Some(b) = bool_opt(p, "grand_totals") {
        pv.grand_total_row = b;
        pv.grand_total_col = b;
        changed = true;
    }
    if let Some(b) = bool_opt(p, "subtotals") {
        pv.subtotals = b;
        changed = true;
    }
    let refresh = bool_opt(p, "refresh").unwrap_or(false);
    if !changed && !refresh {
        return Ok(pivot_info(&pv));
    }
    let overwrite = bool_opt(p, "overwrite").unwrap_or(false);
    let label = if changed { "PivotTable Field" } else { "Refresh" };
    s.transact(label, move |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(slot) = tx
            .wb
            .sheet_mut(sheet)
            .and_then(|sh| sh.analysis.pivots.get_mut(idx))
        {
            *slot = pv;
        }
        refresh_pivot(tx.wb, sheet, idx, overwrite).map_err(failed)?;
        Ok(())
    })?;
    if refresh {
        s.recalc();
    }
    let pv = s
        .wb
        .sheet(sheet)
        .and_then(|sh| sh.analysis.pivots.get(idx))
        .ok_or_else(|| failed("no such PivotTable"))?;
    Ok(pivot_info(pv))
}

// ---------------------------------------------------------------------------------------
// data.consolidate

/// Most source ranges of one consolidation.
const MAX_SOURCES: usize = 255;

fn label_key(v: &Value) -> String {
    v.to_display().trim().to_lowercase()
}

fn consolidate(s: &mut Session, p: &Json) -> CmdResult {
    let home = s.sheet_index();
    let func = match str_opt(p, "func").or_else(|| str_opt(p, "function")) {
        Some(n) => Aggregate::from_name(n).ok_or_else(|| bad(format!("unknown function '{n}'")))?,
        None => Aggregate::Sum,
    };
    let top = bool_opt(p, "top_row").unwrap_or(false);
    let left = bool_opt(p, "left_col").or_else(|| bool_opt(p, "left_column")).unwrap_or(false);
    let srcs = match p.get("sources") {
        Some(Json::Array(a)) if !a.is_empty() && a.len() <= MAX_SOURCES => a,
        Some(Json::Array(_)) => return Err(bad("'sources' needs 1-255 references")),
        _ => return Err(bad("missing list 'sources'")),
    };
    let mut blocks: Vec<Vec<Vec<Value>>> = Vec::with_capacity(srcs.len());
    let mut total: u64 = 0;
    for v in srcs {
        let t = v.as_str().ok_or_else(|| bad("sources are reference texts"))?;
        let (i, r) = resolve_source(&s.wb, home, t).map_err(|_| bad(format!("not a reference: '{t}'")))?;
        total = total.saturating_add(r.cell_count());
        if total > MAX_CELLS_PER_OP {
            return Err(failed("the source ranges are too large"));
        }
        blocks.push(read_block(&s.wb, i, r).map_err(failed)?);
    }
    let (dsheet, dest) = match str_opt(p, "dest") {
        Some(t) => parse_ref_text(&s.wb, home, t).ok_or_else(|| bad(format!("not a reference: '{t}'")))?,
        None => (home, CellRange::single(s.sel.active)),
    };
    let at = dest.start;
    // Row and column keys: labels (first-seen order, case-insensitive) or positions.
    let mut row_keys: Vec<(String, Value)> = Vec::new();
    let mut col_keys: Vec<(String, Value)> = Vec::new();
    let mut accs: std::collections::HashMap<(usize, usize), sp_formula::pivot::Acc> =
        std::collections::HashMap::new();
    let key_index = |keys: &mut Vec<(String, Value)>, k: String, v: Value| -> usize {
        if let Some(i) = keys.iter().position(|(x, _)| *x == k) {
            return i;
        }
        keys.push((k, v));
        keys.len() - 1
    };
    let mut max_rows = 0usize;
    let mut max_cols = 0usize;
    for b in &blocks {
        let header: Option<&Vec<Value>> = if top { b.first() } else { None };
        for (ri, row) in b.iter().enumerate().skip(usize::from(top)) {
            let rk = if left {
                let lab = row.first().cloned().unwrap_or(Value::Empty);
                if lab.is_empty() {
                    continue;
                }
                key_index(&mut row_keys, label_key(&lab), lab)
            } else {
                ri - usize::from(top)
            };
            max_rows = max_rows.max(rk + 1);
            for (ci, v) in row.iter().enumerate().skip(usize::from(left)) {
                let ck = if top {
                    let lab = header.and_then(|h| h.get(ci)).cloned().unwrap_or(Value::Empty);
                    if lab.is_empty() {
                        continue;
                    }
                    key_index(&mut col_keys, label_key(&lab), lab)
                } else {
                    ci - usize::from(left)
                };
                max_cols = max_cols.max(ck + 1);
                accs.entry((rk, ck)).or_default().add(v);
            }
        }
    }
    let nrows = if left { row_keys.len() } else { max_rows };
    let ncols = if top { col_keys.len() } else { max_cols };
    let out_rows = nrows + usize::from(top);
    let out_cols = ncols + usize::from(left);
    if out_rows == 0 || out_cols == 0 {
        return Err(failed("nothing to consolidate"));
    }
    if u64::from(at.row) + out_rows as u64 > u64::from(MAX_ROWS)
        || u64::from(at.col) + out_cols as u64 > u64::from(MAX_COLS)
        || (out_rows as u64).saturating_mul(out_cols as u64) > MAX_CELLS_PER_OP
    {
        return Err(failed("the consolidation does not fit on the sheet"));
    }
    let area = CellRange::new(
        at,
        CellAddr::new(at.row + out_rows as u32 - 1, at.col + out_cols as u32 - 1),
    );
    s.transact("Consolidate", |tx, _| {
        let put = |tx: &mut sp_ops::Tx<'_>, r: usize, c: usize, v: Value| {
            let a = CellAddr::new(at.row + r as u32, at.col + c as u32);
            let style = tx
                .wb
                .sheet(dsheet)
                .and_then(|sh| sh.get(a))
                .map(|c| c.style)
                .unwrap_or_default();
            let cell = if v.is_empty() && style == StyleId::DEFAULT {
                None
            } else {
                Some(Cell {
                    style,
                    ..Cell::constant(v)
                })
            };
            tx.set_cell(dsheet, a, cell);
        };
        if top {
            for (j, (_, v)) in col_keys.iter().enumerate() {
                put(tx, 0, j + usize::from(left), v.clone());
            }
        }
        if left {
            for (i, (_, v)) in row_keys.iter().enumerate() {
                put(tx, i + usize::from(top), 0, v.clone());
            }
        }
        for i in 0..nrows {
            for j in 0..ncols {
                let v = match accs.get(&(i, j)) {
                    Some(a) if a.count > 0 => a.result(func),
                    _ => Value::Empty,
                };
                put(tx, i + usize::from(top), j + usize::from(left), v);
            }
        }
        Ok(())
    })?;
    Ok(json!({"range": area.to_a1(), "function": func.name()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "insert.pivot_table",
            r#"{"source"?: "A1:D20"|"Table1" (default: table at / region around the active cell), "dest"?: "Sheet2!F3" (default: new sheet), "name"?: "PivotTable1", "rows"?: ["Region"], "cols"?: ["Year"], "values"?: [{"field": "Sales", "func"?: "sum|count|average|max|min|product|countNums|stdDev|stdDevp|var|varp", "show_as"?: "normal|percentOfTotal|percentOfRow|percentOfCol", "name"?: "Total"}], "filters"?: ["Product" | {"field": "Product", "items": ["A"]}], "layout"?: "compact|outline|tabular", "grand_totals"?: true, "grand_total_row"?: true, "grand_total_col"?: true, "subtotals"?: true, "style"?: "PivotStyleLight16", "overwrite"?: false}"#,
            insert_pivot_table,
        ),
        CommandSpec::new("data.refresh_all", "{}", refresh_all),
        CommandSpec::new(
            "pivot.field",
            r#"{"pivot"?: "PivotTable1" (default: pivot at the active cell), "field"?: "Region", "area"?: "rows|cols|filters|values|none", "position"?: 0, "check"?: bool (add to Rows, numbers to Values / remove), "remove"?: bool, "value"?: N (a Values entry: with "func"?, "show_as"?, "num_fmt"?, "name"?, "area"?, "position"?, "remove"?), "layout"?: "compact|outline|tabular", "grand_totals"?: bool, "subtotals"?: bool, "refresh"?: bool, "overwrite"?: bool} → the pivot's fields and areas"#,
            pivot_field,
        ),
        CommandSpec::new(
            "insert.slicer",
            r#"{"pivot"?: "PivotTable1" (default: pivot at the active cell), "field": "Region", "name"?: "Slicer_Region", "caption"?: "Region", "select"?: ["East", "West"], "clear"?: true, "anchor"?: "H2:J12", "style"?: "SlicerStyleLight1", "columns"?: 1} | {"name": "Slicer_Region", "select"?: [..], "clear"?: true, "anchor"?: "H2:J12", "style"?, "columns"?, "delete"?: true}"#,
            insert_slicer,
        ),
        CommandSpec::new(
            "data.consolidate",
            r#"{"sources": ["Sheet1!A1:C5", "Sheet2!A1:C7"], "dest"?: "Sheet3!A1" (default: active cell), "func"?: "sum|count|average|max|min|product|countNums|stdDev|stdDevp|var|varp", "top_row"?: false, "left_col"?: false}"#,
            consolidate,
        ),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use crate::Session;
    use serde_json::json;
    use sp_core::{CellAddr, Value};

    fn set(s: &mut Session, a: &str, v: &str) {
        s.execute("cell.set", &json!({"cell": a, "input": v})).unwrap();
    }

    fn val(s: &Session, sheet: usize, a: &str) -> Value {
        s.wb.sheets[sheet].value(CellAddr::parse(a).unwrap()).clone()
    }

    pub(crate) fn sales() -> Session {
        let mut s = Session::new();
        let rows = [
            ["Region", "Product", "Year", "Sales"],
            ["East", "A", "2023", "10"],
            ["East", "B", "2024", "20"],
            ["West", "A", "2023", "30"],
            ["West", "B", "2024", "40"],
            ["East", "A", "2024", "5"],
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let a = CellAddr::new(r as u32, c as u32).to_a1();
                set(&mut s, &a, v);
            }
        }
        s
    }

    #[test]
    fn create_on_existing_sheet_and_undo() {
        let mut s = sales();
        let r = s
            .execute(
                "insert.pivot_table",
                &json!({"source": "A1:D6", "dest": "F1", "rows": ["Region"], "values": ["Sales"]}),
            )
            .unwrap();
        assert_eq!(r["name"], "PivotTable1");
        assert_eq!(val(&s, 0, "F1"), Value::text("Row Labels"));
        assert_eq!(val(&s, 0, "G1"), Value::text("Sum of Sales"));
        assert_eq!(val(&s, 0, "F2"), Value::text("East"));
        assert_eq!(val(&s, 0, "G2"), Value::Number(35.0));
        assert_eq!(val(&s, 0, "G3"), Value::Number(70.0));
        assert_eq!(val(&s, 0, "F4"), Value::text("Grand Total"));
        assert_eq!(val(&s, 0, "G4"), Value::Number(105.0));
        assert_eq!(s.wb.sheets[0].analysis.pivots.len(), 1);
        // GETPIVOTDATA reads the model.
        set(&mut s, "J1", "=GETPIVOTDATA(\"Sales\",F1,\"Region\",\"West\")");
        assert_eq!(val(&s, 0, "J1"), Value::Number(70.0));
        // One undo step removes output and model.
        s.execute("edit.undo", &json!({})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.wb.sheets[0].analysis.pivots.is_empty());
        assert_eq!(val(&s, 0, "F1"), Value::Empty);
    }

    #[test]
    fn new_sheet_columns_filters_and_refuses_overwrite() {
        let mut s = sales();
        let r = s
            .execute(
                "insert.pivot_table",
                &json!({"rows": ["Region"], "cols": ["Year"], "values": [{"field": "Sales", "func": "max"}],
                        "filters": [{"field": "Product", "items": ["A"]}]}),
            )
            .unwrap();
        assert_eq!(s.wb.sheets.len(), 2);
        assert_eq!(s.wb.active, 0);
        let sh = &s.wb.sheets[0];
        assert_eq!(r["sheet"], sh.name.as_str());
        let pv = &sh.analysis.pivots[0];
        assert_eq!(pv.location, "$A$1");
        assert_eq!(val(&s, 0, "A1"), Value::text("Product"));
        assert_eq!(val(&s, 0, "B1"), Value::text("A"));
        // Max of Product A sales: East/2023 = 10, East/2024 = 5, West/2023 = 30.
        assert_eq!(
            sp_formula::pivot::query(pv, "Sales", &[("Region".into(), Value::text("East"))]),
            Value::Number(10.0)
        );
        // Busy destination refused unless overwrite.
        set(&mut s, "Sheet1!H1", "x");
        let e = s.execute(
            "insert.pivot_table",
            &json!({"source": "Sheet1!A1:D6", "dest": "Sheet1!H1", "rows": ["Product"], "values": ["Sales"]}),
        );
        assert!(e.is_err());
        s.execute(
            "insert.pivot_table",
            &json!({"source": "Sheet1!A1:D6", "dest": "Sheet1!H1", "rows": ["Product"], "values": ["Sales"], "overwrite": true}),
        )
        .unwrap();
        assert_eq!(val(&s, 1, "H1"), Value::text("Row Labels"));
        // Bad params.
        for bad in [
            json!({"source": "Sheet1!A1:D6", "rows": ["Nope"]}),
            json!({"source": "Sheet1!A1:D6", "rows": ["Region"], "cols": ["Region"]}),
            json!({"source": "Sheet1!A1:D6", "values": [{"field": "Sales", "func": "median"}]}),
            json!({"source": "Sheet1!Z1:Z1"}),
        ] {
            assert!(s.execute("insert.pivot_table", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn refresh_all_picks_up_source_edits() {
        let mut s = sales();
        s.execute(
            "insert.pivot_table",
            &json!({"source": "A1:D6", "dest": "F1", "rows": ["Region"], "values": ["Sales"]}),
        )
        .unwrap();
        set(&mut s, "D2", "100");
        assert_eq!(val(&s, 0, "G2"), Value::Number(35.0));
        let r = s.execute("data.refresh_all", &json!({})).unwrap();
        assert_eq!(r["refreshed"], 1);
        assert_eq!(val(&s, 0, "G2"), Value::Number(125.0));
        // A renamed source column drops out of the definition but refresh still works.
        set(&mut s, "D1", "Revenue");
        s.execute("data.refresh_all", &json!({})).unwrap();
        assert!(s.wb.sheets[0].analysis.pivots[0].values.is_empty());
        // Nothing to refresh is fine.
        let mut empty = Session::new();
        assert_eq!(empty.execute("data.refresh_all", &json!({})).unwrap()["refreshed"], 0);
    }

    #[test]
    fn fields_pane_edits_go_through_pivot_field() {
        let mut s = sales();
        s.execute(
            "insert.pivot_table",
            &json!({"source": "A1:D6", "dest": "F1", "values": []}),
        )
        .unwrap();
        s.select(CellAddr::parse("F1").unwrap());
        let info = s.execute("pivot.field", &json!({})).unwrap();
        assert_eq!(info["fields"].as_array().unwrap().len(), 4);
        assert_eq!(info["fields"][3]["numeric"], true);
        // Checking a text field adds it to Rows, a numeric one to Values.
        s.execute("pivot.field", &json!({"field": "Region", "check": true})).unwrap();
        let info = s.execute("pivot.field", &json!({"field": "Sales", "check": true})).unwrap();
        assert_eq!(info["rows"], json!(["Region"]));
        assert_eq!(info["values"][0]["caption"], "Sum of Sales");
        assert_eq!(val(&s, 0, "F2"), Value::text("East"));
        assert_eq!(val(&s, 0, "G2"), Value::Number(35.0));
        // Drag Year to Columns, then Region from Rows to Filters at the top.
        s.execute("pivot.field", &json!({"field": "Year", "area": "cols"})).unwrap();
        let info = s
            .execute("pivot.field", &json!({"field": "Region", "area": "filters", "position": 0}))
            .unwrap();
        assert_eq!(info["filters"], json!(["Region"]));
        assert_eq!(info["rows"], json!([]));
        // Value field settings: average, number format, caption.
        let info = s
            .execute(
                "pivot.field",
                &json!({"value": 0, "func": "average", "num_fmt": "0.0", "name": "Avg Sales"}),
            )
            .unwrap();
        assert_eq!(info["values"][0]["func"], "average");
        let pv = &s.wb.sheets[0].analysis.pivots[0];
        assert_eq!(pv.values[0].num_fmt, "0.0");
        // The value cells carry the number format.
        let out = super::parse_ref_text(&s.wb, 0, &pv.output).unwrap().1;
        let fmt_cells = s.wb.sheets[0]
            .cells_in(out)
            .filter(|(_, c)| s.wb.styles.get(c.style).num_fmt == "0.0")
            .count();
        assert!(fmt_cells > 0);
        // Show values as % of total, then move the value field to Rows (drops it).
        s.execute("pivot.field", &json!({"value": 0, "show_as": "percentOfTotal"})).unwrap();
        s.execute("pivot.field", &json!({"value": 0, "area": "rows"})).unwrap();
        let pv = &s.wb.sheets[0].analysis.pivots[0];
        assert!(pv.values.is_empty());
        assert_eq!(pv.rows, vec![3]);
        // Remove, refresh, undo.
        s.execute("pivot.field", &json!({"field": "Sales", "area": "none"})).unwrap();
        assert!(s.wb.sheets[0].analysis.pivots[0].rows.is_empty());
        s.execute("pivot.field", &json!({"refresh": true})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.wb.sheets[0].analysis.pivots[0].rows, vec![3]);
        for bad in [
            json!({"field": "Nope", "area": "rows"}),
            json!({"field": "Region", "area": "sideways"}),
            json!({"value": 9}),
            json!({"pivot": "Nope"}),
            json!({"layout": "round"}),
        ] {
            assert!(s.execute("pivot.field", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn slicer_anchor_state_and_move() {
        let mut s = sales();
        s.execute(
            "insert.pivot_table",
            &json!({"source": "A1:D6", "dest": "F1", "rows": ["Product"], "values": ["Sales"]}),
        )
        .unwrap();
        let r = s.execute("insert.slicer", &json!({"field": "Region"})).unwrap();
        let sl = s.wb.sheets[0].analysis.slicers[0].clone();
        // Placed to the right of the output (F1:G4 → from column I).
        assert_eq!(sl.anchor.from, CellAddr::new(0, 8));
        assert_eq!(r["anchor"], "I1:K6");
        s.execute("insert.slicer", &json!({"name": "Slicer_Region", "anchor": "B10:C20"}))
            .unwrap();
        assert_eq!(s.wb.sheets[0].analysis.slicers[0].anchor.from, CellAddr::new(9, 1));
        assert_eq!(s.history.undo_label(), Some("Move Slicer"));
        // A second slicer on Year, then filter Region to West: 2023 still has data, an
        // item with no remaining records shows as such.
        s.execute("insert.slicer", &json!({"field": "Year"})).unwrap();
        s.execute("insert.slicer", &json!({"name": "Slicer_Region", "select": ["West"]}))
            .unwrap();
        s.execute("insert.slicer", &json!({"name": "Slicer_Year", "select": ["2024"]}))
            .unwrap();
        let pv = &s.wb.sheets[0].analysis.pivots[0];
        let region = super::slicer_state(pv, 0);
        assert_eq!(region.iter().map(|i| i.selected).collect::<Vec<_>>(), vec![false, true]);
        assert!(region.iter().all(|i| i.has_data));
        // Product B only exists in 2024: with Year = 2023 it would have no data.
        s.execute("insert.slicer", &json!({"name": "Slicer_Year", "select": ["2023"]}))
            .unwrap();
        let pv = &s.wb.sheets[0].analysis.pivots[0];
        let product = super::slicer_state(pv, 1);
        assert_eq!(product[1].label, "B");
        assert!(!product[1].has_data);
        assert!(s.execute("insert.slicer", &json!({"name": "Slicer_Region", "anchor": 5})).is_err());
        assert!(s.execute("insert.slicer", &json!({"name": "Slicer_Region", "columns": 0})).is_err());
    }

    #[test]
    fn slicer_filters_pivot() {
        let mut s = sales();
        s.execute(
            "insert.pivot_table",
            &json!({"source": "A1:D6", "dest": "F1", "rows": ["Product"], "values": ["Sales"]}),
        )
        .unwrap();
        let r = s
            .execute("insert.slicer", &json!({"field": "Region", "select": ["West"]}))
            .unwrap();
        assert_eq!(r["name"], "Slicer_Region");
        assert_eq!(r["items"][0]["selected"], false);
        assert_eq!(r["items"][1]["selected"], true);
        // Product A West = 30, B West = 40; grand total 70.
        assert_eq!(val(&s, 0, "G2"), Value::Number(30.0));
        assert_eq!(val(&s, 0, "G4"), Value::Number(70.0));
        s.execute("insert.slicer", &json!({"name": "Slicer_Region", "clear": true}))
            .unwrap();
        assert_eq!(val(&s, 0, "G4"), Value::Number(105.0));
        assert!(
            s.execute("insert.slicer", &json!({"name": "Slicer_Region", "select": []}))
                .is_err()
        );
        s.execute("insert.slicer", &json!({"name": "Slicer_Region", "delete": true}))
            .unwrap();
        assert!(s.wb.sheets[0].analysis.slicers.is_empty());
        assert!(s.execute("insert.slicer", &json!({"field": "Nope"})).is_err());
        assert!(Session::new().execute("insert.slicer", &json!({"field": "Region"})).is_err());
    }

    #[test]
    fn consolidate_by_position_and_labels() {
        let mut s = Session::new();
        for (a, v) in [("A1", "1"), ("B1", "2"), ("A2", "3"), ("D1", "10"), ("E1", "20"), ("D2", "30"), ("E2", "40")] {
            set(&mut s, a, v);
        }
        s.execute(
            "data.consolidate",
            &json!({"sources": ["A1:B2", "D1:E2"], "dest": "G1"}),
        )
        .unwrap();
        assert_eq!(val(&s, 0, "G1"), Value::Number(11.0));
        assert_eq!(val(&s, 0, "H1"), Value::Number(22.0));
        assert_eq!(val(&s, 0, "G2"), Value::Number(33.0));
        assert_eq!(val(&s, 0, "H2"), Value::Number(40.0));

        let mut s = Session::new();
        for (a, v) in [
            ("A1", "Item"), ("B1", "Q1"), ("A2", "Pens"), ("B2", "5"), ("A3", "Ink"), ("B3", "2"),
            ("D1", "Item"), ("E1", "Q2"), ("F1", "Q1"), ("D2", "ink"), ("E2", "7"), ("F2", "1"),
            ("D3", "Paper"), ("E3", "4"),
        ] {
            set(&mut s, a, v);
        }
        s.execute(
            "data.consolidate",
            &json!({"sources": ["A1:B3", "D1:F3"], "dest": "H1", "top_row": true, "left_col": true, "func": "sum"}),
        )
        .unwrap();
        assert_eq!(val(&s, 0, "I1"), Value::text("Q1"));
        assert_eq!(val(&s, 0, "J1"), Value::text("Q2"));
        assert_eq!(val(&s, 0, "H2"), Value::text("Pens"));
        assert_eq!(val(&s, 0, "H3"), Value::text("Ink"));
        assert_eq!(val(&s, 0, "H4"), Value::text("Paper"));
        assert_eq!(val(&s, 0, "I3"), Value::Number(3.0));
        assert_eq!(val(&s, 0, "J3"), Value::Number(7.0));
        assert_eq!(val(&s, 0, "I4"), Value::Empty);
        assert!(s.execute("data.consolidate", &json!({"sources": []})).is_err());
        assert!(s.execute("data.consolidate", &json!({"sources": ["A1"], "func": "x"})).is_err());
    }
}
