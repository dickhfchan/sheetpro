//! Pivot tables in .xlsx: `pivotTableDefinition`, `pivotCacheDefinition` and
//! `pivotCacheRecords` parts (ECMA-376 Part 1, §18.10).
//!
//! Same rule as charts: adopt what we fully model, pass the rest through.
//! - **Load**: after capture, [`adopt_captured_pivots`] parses every captured pivot table. If
//!   *every* pivot of the workbook is modelled (worksheet/table source, plain fields, no
//!   grouping, calculated fields/items, label/value filters, pivot charts, slicers or
//!   timelines, data fields shown as normal or a percentage of a total), they move into
//!   `Sheet::analysis.pivots` and their parts, relationships and the workbook `pivotCaches`
//!   element leave the pass-through. Otherwise nothing is adopted.
//! - **Save**: [`PivotPlan`] names the parts around every captured part name. A pivot still
//!   equal to what was loaded is written back verbatim (original part names and `cacheId`);
//!   any other is generated from the model with `refreshOnLoad` set. The workbook
//!   `pivotCaches` element lists ours plus any captured caches.

use crate::xlsx_extras::SheetRels;
use crate::xlsx_passthrough::{REL_NS, rel_ids_in, rel_kind};
use crate::xml::{Node, esc, parse_dom};
use quick_xml::Reader;
use quick_xml::events::Event;
use sp_core::addr::{col_to_letters, quote_sheet_name, split_sheet_prefix};
use sp_core::{CellAddr, CellRange, ErrorKind, MAX_COLS, MAX_ROWS, Value};
use sp_formula::pivot::{CellKind, ItemKey, cmp_items, item_key};
use sp_model::Workbook;
use sp_model::passthrough::{RawPart, RawRel};
use sp_model::pivot::{
    Aggregate, DataField, FieldFilter, MAX_FIELDS, MAX_OBJECTS, MAX_RECORDS, PivotCache,
    PivotLayout, PivotOriginal, PivotTable, ShowAs,
};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

pub const TABLE_CT: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml";
pub const CACHE_CT: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml";
pub const RECORDS_CT: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheRecords+xml";

/// Largest records part parsed for adoption (bigger ones stay passed through).
const MAX_RECORDS_BYTES: usize = 256 << 20;

// ---------------------------------------------------------------------------------------
// Load

fn find_part<'a>(parts: &[&'a RawPart], path: &str) -> Option<&'a RawPart> {
    parts.iter().copied().find(|p| p.path == path)
}

fn rel_target<'a>(part: &'a RawPart, kind: &str) -> Option<&'a RawRel> {
    part.rels
        .iter()
        .find(|r| !r.external && rel_kind(&r.rel_type) == kind)
}

fn parse_item(n: &Node) -> Option<Value> {
    let v = n.attr("v");
    Some(match n.name.as_str() {
        "m" => Value::Empty,
        "s" => Value::text(v.unwrap_or("")),
        "n" => Value::Number(v?.trim().parse::<f64>().ok().filter(|x| x.is_finite())?),
        "b" => Value::Bool(matches!(v?, "1" | "true")),
        "e" => Value::Error(ErrorKind::parse(v?)?),
        "d" => Value::Number(iso_serial(v?)?),
        _ => return None,
    })
}

/// `2023-01-31T00:00:00` → date serial.
fn iso_serial(s: &str) -> Option<f64> {
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    let mut serial = sp_core::date::date_to_serial(y, m, d)?;
    if !time.is_empty() {
        let mut t = time.split(':');
        let h: f64 = t.next()?.parse().ok()?;
        let mi: f64 = t.next().unwrap_or("0").parse().ok()?;
        let se: f64 = t.next().unwrap_or("0").trim_end_matches('Z').parse().ok()?;
        serial += (h * 3600.0 + mi * 60.0 + se) / 86_400.0;
    }
    Some(serial)
}

struct CacheDef {
    source: String,
    fields: Vec<String>,
    shared: Vec<Vec<Value>>,
}

fn parse_cache(xml: &str, home_sheet: &str) -> Option<CacheDef> {
    let root = parse_dom(xml).ok()?;
    if root.name != "pivotCacheDefinition" {
        return None;
    }
    let src = root.child("cacheSource")?;
    if src.attr("type").unwrap_or("worksheet") != "worksheet" {
        return None;
    }
    if src.children.iter().any(|c| c.name == "consolidation") {
        return None;
    }
    let ws = src.child("worksheetSource")?;
    if ws.attr("id").is_some() {
        return None;
    }
    let source = match (ws.attr("name"), ws.attr("ref")) {
        (Some(n), _) if !n.is_empty() => {
            if n.contains('!') || n.contains('[') {
                return None;
            }
            n.to_string()
        }
        (_, Some(r)) => {
            let range = CellRange::parse(&r.replace('$', "")).ok()?;
            let sheet = ws.attr("sheet").unwrap_or(home_sheet);
            format!("{}!{}", quote_sheet_name(sheet), abs_ref(range))
        }
        _ => return None,
    };
    let cf = root.child("cacheFields")?;
    let mut fields = Vec::new();
    let mut shared = Vec::new();
    for f in cf.children_named("cacheField") {
        if fields.len() >= MAX_FIELDS
            || f.attr("formula").is_some()
            || f.attr("databaseField") == Some("0")
            || f.child("fieldGroup").is_some()
            || f.child("mpMap").is_some()
        {
            return None;
        }
        let name = f.attr("name")?.to_string();
        if name.trim().is_empty()
            || fields
                .iter()
                .any(|x: &String| x.eq_ignore_ascii_case(&name))
        {
            return None;
        }
        let mut items = Vec::new();
        if let Some(si) = f.child("sharedItems") {
            for it in &si.children {
                items.push(parse_item(it)?);
            }
        }
        fields.push(name);
        shared.push(items);
    }
    if fields.is_empty() {
        return None;
    }
    Some(CacheDef {
        source,
        fields,
        shared,
    })
}

/// Streaming parse of `pivotCacheRecords`.
fn parse_records(xml: &str, def: &CacheDef) -> Option<Vec<Vec<Value>>> {
    let mut reader = Reader::from_str(xml);
    let width = def.fields.len();
    let mut out: Vec<Vec<Value>> = Vec::new();
    let mut cur: Option<Vec<Value>> = None;
    loop {
        let ev = reader.read_event().ok()?;
        let (e, empty) = match &ev {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) => {
                if crate::xml::local(e.name().as_ref()) == "r" {
                    let r = cur.take()?;
                    if r.len() != width {
                        return None;
                    }
                    out.push(r);
                    if out.len() > MAX_RECORDS {
                        return None;
                    }
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let name = crate::xml::local(e.name().as_ref()).to_string();
        match name.as_str() {
            "pivotCacheRecords" => {}
            "r" => {
                if cur.is_some() {
                    return None;
                }
                if empty {
                    if width != 0 {
                        return None;
                    }
                    out.push(Vec::new());
                } else {
                    cur = Some(Vec::with_capacity(width));
                }
            }
            "x" | "m" | "s" | "n" | "b" | "e" | "d" => {
                let r = cur.as_mut()?;
                let attrs = crate::xml::attrs(e);
                let node = Node {
                    name: name.clone(),
                    attrs,
                    ..Node::default()
                };
                let v = if name == "x" {
                    let i: usize = node.attr("v").unwrap_or("0").parse().ok()?;
                    def.shared.get(r.len())?.get(i)?.clone()
                } else {
                    parse_item(&node)?
                };
                r.push(v);
                if !empty {
                    // Items may carry children (`tpls`, `x` member properties): unsupported.
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

fn yes(n: &Node, k: &str, default: bool) -> bool {
    match n.attr(k) {
        Some(v) => matches!(v, "1" | "true"),
        None => default,
    }
}

/// Parse a pivot table definition against its cache. `None` = not modelled.
fn parse_table(xml: &str, def: &CacheDef, records: Vec<Vec<Value>>) -> Option<(PivotTable, u32)> {
    let root = parse_dom(xml).ok()?;
    if root.name != "pivotTableDefinition" {
        return None;
    }
    for unsupported in [
        "filters",
        "calculatedItems",
        "calculatedMembers",
        "chartFormats",
        "pivotHierarchies",
        "rowHierarchiesUsage",
        "colHierarchiesUsage",
    ] {
        if root.child(unsupported).is_some() {
            return None;
        }
    }
    if yes(&root, "dataOnRows", false) {
        return None;
    }
    let cache_id: u32 = root.attr("cacheId")?.parse().ok()?;
    let nf = def.fields.len();
    let mut p = PivotTable {
        name: root.attr("name")?.to_string(),
        source: def.source.clone(),
        cache: PivotCache {
            fields: def.fields.clone(),
            records,
        },
        grand_total_col: yes(&root, "rowGrandTotals", true),
        grand_total_row: yes(&root, "colGrandTotals", true),
        ..PivotTable::default()
    };
    if let Some(st) = root
        .child("pivotTableStyleInfo")
        .and_then(|n| n.attr("name"))
    {
        st.clone_into(&mut p.style);
    }
    // Fields: item lists (to map item indexes) and hidden items.
    let pfs: Vec<&Node> = root
        .child("pivotFields")?
        .children_named("pivotField")
        .collect();
    if pfs.len() != nf {
        return None;
    }
    let mut item_lists: Vec<Vec<Option<usize>>> = Vec::with_capacity(nf);
    for (f, pf) in pfs.iter().enumerate() {
        if pf.child("autoSortScope").is_some()
            || matches!(pf.attr("sortType"), Some("descending"))
            || pf.attr("dataSourceSort").is_some_and(|v| v == "1")
        {
            return None;
        }
        for custom in [
            "sumSubtotal",
            "countASubtotal",
            "avgSubtotal",
            "maxSubtotal",
            "minSubtotal",
            "productSubtotal",
            "countSubtotal",
            "stdDevSubtotal",
            "stdDevPSubtotal",
            "varSubtotal",
            "varPSubtotal",
        ] {
            if yes(pf, custom, false) {
                return None;
            }
        }
        let shared = def.shared.get(f)?;
        let mut list = Vec::new();
        let mut hidden = Vec::new();
        if let Some(items) = pf.child("items") {
            for it in items.children_named("item") {
                match it.attr("t") {
                    None | Some("data") => {}
                    Some("default") => {
                        list.push(None);
                        continue;
                    }
                    Some(_) => return None,
                }
                if it.attr("f").is_some_and(|v| v == "1") {
                    return None;
                }
                let x: usize = it.attr("x")?.parse().ok()?;
                let v = shared.get(x)?.clone();
                list.push(Some(x));
                if yes(it, "h", false) {
                    hidden.push(v);
                }
            }
        }
        if !hidden.is_empty() {
            p.field_filters.push(FieldFilter { field: f, hidden });
        }
        item_lists.push(list);
    }
    let fields_of = |name: &str| -> Option<Vec<i64>> {
        match root.child(name) {
            None => Some(Vec::new()),
            Some(n) => n
                .children_named("field")
                .map(|x| x.attr("x").and_then(|v| v.parse::<i64>().ok()))
                .collect(),
        }
    };
    let rows = fields_of("rowFields")?;
    let mut cols = fields_of("colFields")?;
    if rows.contains(&-2) {
        return None;
    }
    if let Some(pos) = cols.iter().position(|x| *x == -2) {
        if pos + 1 != cols.len() {
            return None;
        }
        cols.pop();
    }
    let idx = |x: &i64| -> Option<usize> { usize::try_from(*x).ok().filter(|i| *i < nf) };
    p.rows = rows.iter().map(idx).collect::<Option<_>>()?;
    p.cols = cols.iter().map(idx).collect::<Option<_>>()?;
    if let Some(pg) = root.child("pageFields") {
        for pf in pg.children_named("pageField") {
            let f: usize = pf.attr("fld")?.parse().ok().filter(|i| *i < nf)?;
            if let Some(item) = pf.attr("item") {
                // A single selected item (index into the field's item list).
                let k: usize = item.parse().ok()?;
                let list = item_lists.get(f)?;
                let chosen = (*list.get(k)?)?;
                let shared = def.shared.get(f)?;
                let hidden: Vec<Value> = shared
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != chosen)
                    .map(|(_, v)| v.clone())
                    .collect();
                p.field_filters.retain(|ff| ff.field != f);
                if !hidden.is_empty() {
                    p.field_filters.push(FieldFilter { field: f, hidden });
                }
            }
            p.filters.push(f);
        }
    }
    if let Some(dfs) = root.child("dataFields") {
        for df in dfs.children_named("dataField") {
            let f: usize = df.attr("fld")?.parse().ok().filter(|i| *i < nf)?;
            let func = Aggregate::from_name(df.attr("subtotal").unwrap_or("sum"))?;
            let show_as = match df.attr("showDataAs") {
                None => ShowAs::Normal,
                Some(s) => match ShowAs::from_name(s)? {
                    ShowAs::Normal if s != "normal" => return None,
                    v => v,
                },
            };
            let mut name = df.attr("name").unwrap_or("").to_string();
            // A caption equal to the generated one is kept implicit.
            let field_name = def.fields.get(f).map_or("", String::as_str);
            if name == format!("{} of {field_name}", func.caption()) {
                name.clear();
            }
            p.values.push(DataField {
                field: f,
                func,
                show_as,
                name,
            });
        }
    }
    // Layout and subtotals from the first axis field (else the table defaults).
    let first = p.rows.first().or(p.cols.first()).and_then(|f| pfs.get(*f));
    let compact = yes(&root, "compact", true) && first.is_none_or(|n| yes(n, "compact", true));
    let outline = first.map_or(yes(&root, "outline", false), |n| yes(n, "outline", true));
    p.layout = if compact && outline {
        PivotLayout::Compact
    } else if outline {
        PivotLayout::Outline
    } else {
        PivotLayout::Tabular
    };
    let axis: Vec<&usize> = p.rows.iter().chain(&p.cols).collect();
    p.subtotals = axis.is_empty()
        || axis.iter().any(|f| {
            pfs.get(**f)
                .is_some_and(|n| yes(n, "defaultSubtotal", true))
        });
    // Location: the table area; the filter area (one row per page field + a gap) above it.
    let loc = root.child("location")?;
    let r = CellRange::parse(&loc.attr("ref")?.replace('$', "")).ok()?;
    let pages = p.filters.len();
    let top = if pages > 0 {
        r.start.row.saturating_sub(pages as u32 + 1)
    } else {
        r.start.row
    };
    let start = CellAddr::new(top, r.start.col);
    let end_col = if pages > 0 {
        r.end
            .col
            .max(r.start.col.saturating_add(1))
            .min(MAX_COLS - 1)
    } else {
        r.end.col
    };
    p.location = abs_ref(CellRange::single(start));
    p.output = abs_ref(CellRange::new(start, CellAddr::new(r.end.row, end_col)));
    Some((p, cache_id))
}

/// Cache ids listed by a `pivotCaches` element, with their relationship ids.
fn pivot_caches(xml: &str) -> Option<Vec<(u32, String)>> {
    let root = parse_dom(xml).ok()?;
    root.children_named("pivotCache")
        .map(|c| Some((c.attr("cacheId")?.parse().ok()?, c.attr("id")?.to_string())))
        .collect()
}

/// Adopt every captured pivot table when all of them are modelled (see module docs).
pub fn adopt_captured_pivots(wb: &mut Workbook) {
    let Some(plan) = adoption(wb) else {
        return;
    };
    let mut gone: HashSet<String> = HashSet::new();
    for (sheet, rid, p) in plan {
        if let Some(o) = &p.original {
            gone.insert(o.table_path.clone());
            gone.insert(o.cache_path.clone());
            gone.insert(o.records_path.clone());
        }
        if let Some(sh) = wb.sheets.get_mut(sheet) {
            sh.passthrough.rels.retain(|r| r.id != rid);
            sh.analysis.pivots.push(p);
        }
    }
    for sh in &mut wb.sheets {
        sh.passthrough.parts.retain(|p| !gone.contains(&p.path));
    }
    let wp = &mut wb.passthrough;
    wp.parts.retain(|p| !gone.contains(&p.path));
    wp.elements
        .retain(|e| crate::xml::local(&e.name) != "pivotCaches");
    wp.rels
        .retain(|r| rel_kind(&r.rel_type) != "pivotCacheDefinition");
}

/// (sheet, sheet relationship id, pivot) for every captured pivot table, or `None` when
/// any of them (or anything tied to them) is not modelled.
fn adoption(wb: &Workbook) -> Option<Vec<(usize, String, PivotTable)>> {
    let wp = &wb.passthrough;
    // Slicers, timelines and OLAP pivots tie into caches in ways we do not model.
    let tied =
        |t: &str| t.contains("slicer") || t.contains("timeline") || t.contains("connections");
    if wp
        .rels
        .iter()
        .any(|r| tied(&rel_kind(&r.rel_type).to_ascii_lowercase()))
        || wp.elements.iter().any(|e| {
            crate::xml::local(&e.name) != "pivotCaches"
                && (e.xml.contains("pivotCache")
                    || e.xml.contains("slicerCache")
                    || e.xml.contains("timelineCache"))
        })
    {
        let any = wb.sheets.iter().any(|s| {
            s.passthrough
                .rels
                .iter()
                .any(|r| rel_kind(&r.rel_type) == "pivotTable")
        });
        if any {
            return None;
        }
    }
    let pivot_chart = |p: &RawPart| {
        p.content_type.contains("chart") && p.data.windows(11).any(|w| w == b"pivotSource")
    };
    if wp
        .parts
        .iter()
        .chain(wb.sheets.iter().flat_map(|s| s.passthrough.parts.iter()))
        .any(pivot_chart)
    {
        return None;
    }
    let caches_el = wp
        .elements
        .iter()
        .find(|e| crate::xml::local(&e.name) == "pivotCaches");
    let caches = match caches_el {
        Some(e) => pivot_caches(&e.xml)?,
        None => Vec::new(),
    };
    let mut out = Vec::new();
    let mut used_caches: HashSet<u32> = HashSet::new();
    for (i, sh) in wb.sheets.iter().enumerate() {
        let pivots: Vec<&RawRel> = sh
            .passthrough
            .rels
            .iter()
            .filter(|r| rel_kind(&r.rel_type) == "pivotTable")
            .collect();
        if pivots.is_empty() {
            continue;
        }
        if sh.passthrough.sheet_part.is_some()
            || sh.analysis.pivots.len() + pivots.len() > MAX_OBJECTS
            || sh
                .passthrough
                .elements
                .iter()
                .any(|e| e.xml.contains("slicer") || e.xml.contains("timeline"))
        {
            return None;
        }
        let parts: Vec<&RawPart> = sh.passthrough.parts.iter().chain(wp.parts.iter()).collect();
        for rel in pivots {
            if rel.external {
                return None;
            }
            let table = find_part(&parts, &rel.target)?;
            let cache_rel = rel_target(table, "pivotCacheDefinition")?;
            if table.rels.len() != 1 {
                return None;
            }
            let cache = find_part(&parts, &cache_rel.target)?;
            let rec_rel = rel_target(cache, "pivotCacheRecords")?;
            if cache.rels.len() != 1 {
                return None;
            }
            let records = find_part(&parts, &rec_rel.target)?;
            if records.data.len() > MAX_RECORDS_BYTES || !records.rels.is_empty() {
                return None;
            }
            let cache_xml = std::str::from_utf8(&cache.data).ok()?;
            let def = parse_cache(cache_xml, &sh.name)?;
            let recs = parse_records(std::str::from_utf8(&records.data).ok()?, &def)?;
            let (mut p, cache_id) =
                parse_table(std::str::from_utf8(&table.data).ok()?, &def, recs)?;
            // The workbook must register this cache under the id the table uses.
            let (_, wrid) = caches.iter().find(|(id, _)| *id == cache_id)?;
            let wrel = wp.rels.iter().find(|r| r.id == *wrid)?;
            if wrel.target != cache.path {
                return None;
            }
            used_caches.insert(cache_id);
            p.original = Some(Arc::new(PivotOriginal {
                model: p.clone(),
                table_xml: table.data.clone(),
                cache_xml: cache.data.clone(),
                records_xml: Some(records.data.clone()),
                table_path: table.path.clone(),
                cache_path: cache.path.clone(),
                records_path: records.path.clone(),
                cache_id,
            }));
            out.push((i, rel.id.clone(), p));
        }
    }
    // Every registered cache must belong to an adopted pivot.
    if caches.iter().any(|(id, _)| !used_caches.contains(id)) {
        return None;
    }
    if out.is_empty() { None } else { Some(out) }
}

// ---------------------------------------------------------------------------------------
// Save

fn abs_ref(r: CellRange) -> String {
    let a = |c: CellAddr| format!("${}${}", col_to_letters(c.col), c.row + 1);
    if r.is_single() {
        a(r.start)
    } else {
        format!("{}:{}", a(r.start), a(r.end))
    }
}

struct Entry {
    sheet: usize,
    table_path: String,
    cache_path: String,
    records_path: String,
    cache_id: u32,
    table: Vec<u8>,
    cache: Vec<u8>,
    records: Vec<u8>,
    /// Relationship id the cache definition uses for its records.
    records_rid: String,
}

/// Part names, cache ids and XML of every pivot written.
pub struct PivotPlan {
    entries: Vec<Entry>,
    /// The captured `pivotCaches` element's children, verbatim.
    captured_caches: String,
}

fn records_rid_of(cache_xml: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(cache_xml).ok()?;
    let root = parse_dom(text).ok()?;
    root.attr("id").map(str::to_string)
}

fn cache_ids_in(xml: &str) -> Vec<u32> {
    pivot_caches(xml)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

fn inner_xml(xml: &str) -> &str {
    let open_end = xml.find('>').map_or(0, |i| i + 1);
    if xml.get(..open_end).is_some_and(|h| h.ends_with("/>")) {
        return "";
    }
    let close = xml.rfind("</").unwrap_or(xml.len());
    xml.get(open_end..close).unwrap_or("")
}

impl PivotPlan {
    pub fn new(wb: &Workbook) -> PivotPlan {
        let mut taken: HashSet<String> = wb
            .passthrough
            .parts
            .iter()
            .chain(wb.sheets.iter().flat_map(|s| s.passthrough.parts.iter()))
            .map(|p| p.path.clone())
            .collect();
        let captured = wb
            .passthrough
            .elements
            .iter()
            .find(|e| crate::xml::local(&e.name) == "pivotCaches");
        let mut ids: HashSet<u32> = captured
            .map(|e| cache_ids_in(&e.xml))
            .unwrap_or_default()
            .into_iter()
            .collect();
        let captured_caches = captured.map_or(String::new(), |e| inner_xml(&e.xml).to_string());
        // Pristine pivots keep their names and ids (claimed first).
        let mut pristine: Vec<(usize, usize)> = Vec::new();
        for (i, sh) in wb.sheets.iter().enumerate() {
            if sh.passthrough.sheet_part.is_some() {
                continue;
            }
            for (j, p) in sh.analysis.pivots.iter().enumerate() {
                let Some(o) = &p.original else { continue };
                let free = [&o.table_path, &o.cache_path, &o.records_path]
                    .iter()
                    .all(|n| !n.is_empty() && !taken.contains(*n));
                if p.is_pristine() && free && o.records_xml.is_some() && !ids.contains(&o.cache_id)
                {
                    taken.insert(o.table_path.clone());
                    taken.insert(o.cache_path.clone());
                    taken.insert(o.records_path.clone());
                    ids.insert(o.cache_id);
                    pristine.push((i, j));
                }
            }
        }
        let mut entries = Vec::new();
        let mut k = 0u32;
        let mut next_id = 0u32;
        for (i, sh) in wb.sheets.iter().enumerate() {
            if sh.passthrough.sheet_part.is_some() {
                continue;
            }
            for (j, p) in sh.analysis.pivots.iter().enumerate() {
                if pristine.contains(&(i, j))
                    && let Some(o) = &p.original
                {
                    entries.push(Entry {
                        sheet: i,
                        table_path: o.table_path.clone(),
                        cache_path: o.cache_path.clone(),
                        records_path: o.records_path.clone(),
                        cache_id: o.cache_id,
                        table: o.table_xml.to_vec(),
                        cache: o.cache_xml.to_vec(),
                        records: o
                            .records_xml
                            .as_deref()
                            .map(<[u8]>::to_vec)
                            .unwrap_or_default(),
                        records_rid: records_rid_of(&o.cache_xml).unwrap_or_else(|| "rId1".into()),
                    });
                    continue;
                }
                let Some((table, cache, records)) = generate(wb, i, p) else {
                    continue;
                };
                let names = loop {
                    k += 1;
                    let n = [
                        format!("xl/pivotTables/pivotTable{k}.xml"),
                        format!("xl/pivotCache/pivotCacheDefinition{k}.xml"),
                        format!("xl/pivotCache/pivotCacheRecords{k}.xml"),
                    ];
                    if n.iter().all(|x| !taken.contains(x)) {
                        break n;
                    }
                };
                for n in &names {
                    taken.insert(n.clone());
                }
                let cache_id = loop {
                    next_id += 1;
                    if !ids.contains(&next_id) {
                        break next_id;
                    }
                };
                ids.insert(cache_id);
                let [table_path, cache_path, records_path] = names;
                entries.push(Entry {
                    sheet: i,
                    table_path,
                    cache_path,
                    records_path,
                    cache_id,
                    table: table
                        .replace("{CACHE_ID}", &cache_id.to_string())
                        .into_bytes(),
                    cache: cache.into_bytes(),
                    records: records.into_bytes(),
                    records_rid: "rId1".into(),
                });
            }
        }
        PivotPlan {
            entries,
            captured_caches,
        }
    }

    /// Every part name this plan writes (reserved before pass-through parts are placed).
    pub fn part_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .flat_map(|e| {
                [&e.table_path, &e.cache_path, &e.records_path]
                    .into_iter()
                    .flat_map(|p| [p.clone(), crate::xlsx_passthrough::rels_path(p)])
            })
            .collect()
    }

    /// Relationships from sheet `i` to its pivot tables; returns every part to write
    /// (path, bytes, content type), the parts' relationship parts included.
    pub fn write_sheet(
        &self,
        i: usize,
        rels: &mut SheetRels,
    ) -> Vec<(String, Vec<u8>, &'static str)> {
        let mut out = Vec::new();
        for e in self.entries.iter().filter(|e| e.sheet == i) {
            rels.add("pivotTable", &format!("/{}", e.table_path), false);
            out.push((e.table_path.clone(), e.table.clone(), TABLE_CT));
            out.push((
                crate::xlsx_passthrough::rels_path(&e.table_path),
                crate::xlsx_passthrough::rels_xml(&[RawRel {
                    id: "rId1".into(),
                    rel_type: format!("{REL_NS}/pivotCacheDefinition"),
                    target: format!("/{}", e.cache_path),
                    external: false,
                }])
                .into_bytes(),
                "",
            ));
            out.push((e.cache_path.clone(), e.cache.clone(), CACHE_CT));
            out.push((
                crate::xlsx_passthrough::rels_path(&e.cache_path),
                crate::xlsx_passthrough::rels_xml(&[RawRel {
                    id: e.records_rid.clone(),
                    rel_type: format!("{REL_NS}/pivotCacheRecords"),
                    target: format!("/{}", e.records_path),
                    external: false,
                }])
                .into_bytes(),
                "",
            ));
            out.push((e.records_path.clone(), e.records.clone(), RECORDS_CT));
        }
        out
    }

    /// The workbook `pivotCaches` element (captured caches first, then ours) and the
    /// relationship ids of captured caches it keeps. `take_id` allocates workbook
    /// relationship ids; ours are pushed onto `wb_rels`.
    pub fn workbook_element(
        &self,
        valid_ids: &HashSet<String>,
        take_id: &mut dyn FnMut() -> String,
        wb_rels: &mut Vec<RawRel>,
    ) -> Option<(String, HashSet<String>)> {
        if self.entries.is_empty() {
            return None;
        }
        let mut keep = HashSet::new();
        let mut x = String::from("<pivotCaches>");
        if !self.captured_caches.is_empty() {
            let ids = rel_ids_in(&self.captured_caches, &["r".to_string()]);
            if ids.iter().all(|i| valid_ids.contains(i)) {
                x.push_str(&self.captured_caches);
                keep.extend(ids);
            }
        }
        for e in &self.entries {
            let id = take_id();
            let _ = write!(x, r#"<pivotCache cacheId="{}" r:id="{id}"/>"#, e.cache_id);
            wb_rels.push(RawRel {
                id,
                rel_type: format!("{REL_NS}/pivotCacheDefinition"),
                target: format!("/{}", e.cache_path),
                external: false,
            });
        }
        x.push_str("</pivotCaches>");
        Some((x, keep))
    }
}

// ---------------------------------------------------------------------------------------
// Generation

fn item_xml(out: &mut String, v: &Value) {
    match v {
        Value::Empty => out.push_str("<m/>"),
        Value::Number(n) => {
            let _ = write!(out, r#"<n v="{n}"/>"#);
        }
        Value::Bool(b) => {
            let _ = write!(out, r#"<b v="{}"/>"#, u8::from(*b));
        }
        Value::Error(e) => {
            let _ = write!(out, r#"<e v="{}"/>"#, esc(e.as_str()));
        }
        other => {
            let _ = write!(out, r#"<s v="{}"/>"#, esc(&other.to_display()));
        }
    }
}

fn shared_attrs(items: &[Value]) -> String {
    let has = |f: &dyn Fn(&Value) -> bool| items.iter().any(f);
    let s = has(&|v| matches!(v, Value::Text(_)));
    let n = has(&|v| matches!(v, Value::Number(_)));
    let b = has(&|v| matches!(v, Value::Bool(_)));
    let e = has(&|v| matches!(v, Value::Error(_)));
    let blank = has(&|v| v.is_empty());
    let mut a = String::new();
    if !s && !blank && !b && !e {
        a.push_str(r#" containsSemiMixedTypes="0""#);
    }
    if !s {
        a.push_str(r#" containsString="0""#);
    }
    if [s, n, b, e].iter().filter(|x| **x).count() > 1 {
        a.push_str(r#" containsMixedTypes="1""#);
    }
    if n {
        a.push_str(r#" containsNumber="1""#);
        let nums: Vec<f64> = items.iter().filter_map(Value::as_number).collect();
        if nums.iter().all(|x| x.fract() == 0.0 && x.abs() < 1e15) {
            a.push_str(r#" containsInteger="1""#);
        }
        let min = nums.iter().copied().fold(f64::INFINITY, f64::min);
        let max = nums.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let _ = write!(a, r#" minValue="{min}" maxValue="{max}""#);
    }
    if blank {
        a.push_str(r#" containsBlank="1""#);
    }
    let _ = write!(a, r#" count="{}""#, items.len());
    a
}

const MAIN_DECL: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

/// (table xml with a `{CACHE_ID}` placeholder, cache definition xml, records xml).
fn generate(wb: &Workbook, sheet: usize, p: &PivotTable) -> Option<(String, String, String)> {
    let out = sp_formula::pivot::layout(p).ok()?;
    let nf = p.cache.fields.len();
    if nf == 0 {
        return None;
    }
    // Shared items per field, sorted the way the report shows them.
    let mut shared: Vec<Vec<Value>> = Vec::with_capacity(nf);
    let mut index: Vec<HashMap<ItemKey, usize>> = Vec::with_capacity(nf);
    for f in 0..nf {
        let mut seen: HashMap<ItemKey, Value> = HashMap::new();
        for r in &p.cache.records {
            let v = r.get(f).cloned().unwrap_or(Value::Empty);
            let v = match v {
                Value::Text(t) if t.is_empty() => Value::Empty,
                v => v,
            };
            seen.entry(item_key(&v)).or_insert(v);
        }
        let mut items: Vec<Value> = seen.into_values().collect();
        items.sort_by(cmp_items);
        index.push(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| (item_key(v), i))
                .collect(),
        );
        shared.push(items);
    }
    // Source.
    let source = {
        let (sh, body) = split_sheet_prefix(p.source.trim());
        match (sh, CellRange::parse(&body.replace('$', ""))) {
            (sheet_name, Ok(r)) => {
                let name = sheet_name
                    .unwrap_or_else(|| wb.sheet(sheet).map_or(String::new(), |s| s.name.clone()));
                format!(
                    r#"<worksheetSource ref="{}" sheet="{}"/>"#,
                    r.to_a1(),
                    esc(&name)
                )
            }
            (None, Err(_)) => format!(r#"<worksheetSource name="{}"/>"#, esc(p.source.trim())),
            (Some(_), Err(_)) => return None,
        }
    };
    let mut cache = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<pivotCacheDefinition {MAIN_DECL} r:id="rId1" refreshOnLoad="1" createdVersion="6" refreshedVersion="6" minRefreshableVersion="3" recordCount="{}"><cacheSource type="worksheet">{source}</cacheSource><cacheFields count="{nf}">"#,
        p.cache.records.len()
    );
    for (f, name) in p.cache.fields.iter().enumerate() {
        let items = shared.get(f)?;
        let _ = write!(
            cache,
            r#"<cacheField name="{}" numFmtId="0"><sharedItems{}>"#,
            esc(name),
            shared_attrs(items)
        );
        for v in items {
            item_xml(&mut cache, v);
        }
        cache.push_str("</sharedItems></cacheField>");
    }
    cache.push_str("</cacheFields></pivotCacheDefinition>");

    let mut records = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<pivotCacheRecords {MAIN_DECL} count="{}">"#,
        p.cache.records.len()
    );
    for r in &p.cache.records {
        records.push_str("<r>");
        for f in 0..nf {
            let v = r.get(f).cloned().unwrap_or(Value::Empty);
            let v = match v {
                Value::Text(t) if t.is_empty() => Value::Empty,
                v => v,
            };
            let x = index
                .get(f)
                .and_then(|m| m.get(&item_key(&v)))
                .copied()
                .unwrap_or(0);
            let _ = write!(records, r#"<x v="{x}"/>"#);
        }
        records.push_str("</r>");
    }
    records.push_str("</pivotCacheRecords>");

    // Table.
    let (_, loc) = {
        let (_, body) = split_sheet_prefix(p.location.trim());
        (0, CellRange::parse(&body.replace('$', "")).ok()?)
    };
    let top = loc.start.row as u64 + out.page_rows as u64;
    let mut last_col = 0usize;
    for r in out.page_rows..out.rows {
        for c in 0..out.cols {
            if out.get(r, c).is_some_and(|x| x.kind != CellKind::Blank) {
                last_col = last_col.max(c);
            }
        }
    }
    let bottom = loc.start.row as u64 + out.rows.max(1) as u64 - 1;
    let right = loc.start.col as u64 + last_col as u64;
    if bottom >= u64::from(MAX_ROWS) || right >= u64::from(MAX_COLS) || top > bottom {
        return None;
    }
    let table_ref = CellRange::new(
        CellAddr::new(top as u32, loc.start.col),
        CellAddr::new(bottom as u32, right as u32),
    );
    let compact = p.layout == PivotLayout::Compact;
    let tabular = p.layout == PivotLayout::Tabular;
    let mut t = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<pivotTableDefinition {MAIN_DECL} name="{}" cacheId="{{CACHE_ID}}" applyNumberFormats="0" applyBorderFormats="0" applyFontFormats="0" applyPatternFormats="0" applyAlignmentFormats="0" applyWidthHeightFormats="1" dataCaption="Values" updatedVersion="6" minRefreshableVersion="3" useAutoFormatting="1" itemPrintTitles="1" createdVersion="6" indent="0"{}{}{} outline="1" outlineData="1" multipleFieldFilters="0">"#,
        esc(&p.name),
        if p.grand_total_col {
            ""
        } else {
            r#" rowGrandTotals="0""#
        },
        if p.grand_total_row {
            ""
        } else {
            r#" colGrandTotals="0""#
        },
        if compact {
            ""
        } else {
            r#" compact="0" compactData="0""#
        },
    );
    let _ = write!(
        t,
        r#"<location ref="{}" firstHeaderRow="1" firstDataRow="{}" firstDataCol="{}"{}/>"#,
        table_ref.to_a1(),
        out.header_rows,
        out.label_cols,
        if p.filters.is_empty() {
            String::new()
        } else {
            format!(r#" rowPageCount="{}" colPageCount="1""#, p.filters.len())
        }
    );
    let _ = write!(t, r#"<pivotFields count="{nf}">"#);
    for f in 0..nf {
        let axis = if p.rows.contains(&f) {
            Some("axisRow")
        } else if p.cols.contains(&f) {
            Some("axisCol")
        } else if p.filters.contains(&f) {
            Some("axisPage")
        } else {
            None
        };
        let is_data = p.values.iter().any(|d| d.field == f);
        let hidden: HashSet<ItemKey> = p.hidden_items(f).iter().map(item_key).collect();
        let items = shared.get(f)?;
        let visible = items
            .iter()
            .filter(|v| !hidden.contains(&item_key(v)))
            .count();
        let multi = axis == Some("axisPage") && !hidden.is_empty() && visible != 1;
        t.push_str("<pivotField");
        if let Some(a) = axis {
            let _ = write!(t, r#" axis="{a}""#);
        }
        if is_data {
            t.push_str(r#" dataField="1""#);
        }
        if !compact {
            t.push_str(r#" compact="0""#);
        }
        if tabular {
            t.push_str(r#" outline="0""#);
        }
        if multi {
            t.push_str(r#" multipleItemSelectionAllowed="1""#);
        }
        if !p.subtotals && axis.is_some_and(|a| a != "axisPage") {
            t.push_str(r#" defaultSubtotal="0""#);
        }
        t.push_str(r#" showAll="0""#);
        let page_single = axis == Some("axisPage") && visible == 1 && !hidden.is_empty();
        if axis.is_some() || !hidden.is_empty() {
            let subtotal = axis.is_some() && (p.subtotals || axis == Some("axisPage"));
            let _ = write!(
                t,
                r#"><items count="{}">"#,
                items.len() + usize::from(subtotal)
            );
            for (i, v) in items.iter().enumerate() {
                let h = hidden.contains(&item_key(v)) && !page_single;
                let _ = write!(t, r#"<item x="{i}"{}/>"#, if h { r#" h="1""# } else { "" });
            }
            if subtotal {
                t.push_str(r#"<item t="default"/>"#);
            }
            t.push_str("</items></pivotField>");
        } else {
            t.push_str("/>");
        }
    }
    t.push_str("</pivotFields>");
    if !p.rows.is_empty() {
        let _ = write!(t, r#"<rowFields count="{}">"#, p.rows.len());
        for f in &p.rows {
            let _ = write!(t, r#"<field x="{f}"/>"#);
        }
        t.push_str("</rowFields>");
    }
    let data_col = p.values.len() > 1;
    if !p.cols.is_empty() || data_col {
        let _ = write!(
            t,
            r#"<colFields count="{}">"#,
            p.cols.len() + usize::from(data_col)
        );
        for f in &p.cols {
            let _ = write!(t, r#"<field x="{f}"/>"#);
        }
        if data_col {
            t.push_str(r#"<field x="-2"/>"#);
        }
        t.push_str("</colFields>");
    }
    if !p.filters.is_empty() {
        let _ = write!(t, r#"<pageFields count="{}">"#, p.filters.len());
        for f in &p.filters {
            let hidden: HashSet<ItemKey> = p.hidden_items(*f).iter().map(item_key).collect();
            let items = shared.get(*f)?;
            let vis: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, v)| !hidden.contains(&item_key(v)))
                .map(|(i, _)| i)
                .collect();
            let item = match vis.as_slice() {
                [one] if !hidden.is_empty() => format!(r#" item="{one}""#),
                _ => String::new(),
            };
            let _ = write!(t, r#"<pageField fld="{f}"{item} hier="-1"/>"#);
        }
        t.push_str("</pageFields>");
    }
    if !p.values.is_empty() {
        let _ = write!(t, r#"<dataFields count="{}">"#, p.values.len());
        for (i, d) in p.values.iter().enumerate() {
            let _ = write!(
                t,
                r#"<dataField name="{}" fld="{}"{}{} baseField="0" baseItem="0"{}/>"#,
                esc(&p.data_caption(i)),
                d.field,
                if d.func == Aggregate::Sum {
                    String::new()
                } else {
                    format!(r#" subtotal="{}""#, d.func.name())
                },
                if d.show_as == ShowAs::Normal {
                    String::new()
                } else {
                    format!(r#" showDataAs="{}""#, d.show_as.name())
                },
                if d.show_as == ShowAs::Normal {
                    ""
                } else {
                    r#" numFmtId="10""#
                }
            );
        }
        t.push_str("</dataFields>");
    }
    let _ = write!(
        t,
        r#"<pivotTableStyleInfo name="{}" showRowHeaders="1" showColHeaders="1" showRowStripes="0" showColStripes="0" showLastColumn="1"/></pivotTableDefinition>"#,
        esc(&p.style)
    );
    Some((t, cache, records))
}

// ---------------------------------------------------------------------------------------
// What-if data tables (`<f t="dataTable">`, §18.3.1.40)

/// Most data-table cells materialized from one `<f t="dataTable">`.
const MAX_TABLE_CELLS: u64 = 1_000_000;

/// Turn loaded `<f t="dataTable" ref dt2D dtr r1 r2>` records into `TABLE()` cells over
/// their area (cached values kept) plus a `DataTable` model entry.
pub fn apply_data_tables(sheet: &mut sp_model::Sheet, tables: &[Vec<(String, String)>]) {
    for attrs in tables {
        let get = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        let flag = |k: &str| get(k).is_some_and(|v| v == "1" || v == "true");
        let Some(body) = get("ref").and_then(|r| CellRange::parse(&r.replace('$', "")).ok()) else {
            continue;
        };
        if body.cell_count() > MAX_TABLE_CELLS || body.start.row == 0 || body.start.col == 0 {
            continue;
        }
        let cell = |k: &str| get(k).and_then(|r| CellAddr::parse(&r.replace('$', "")).ok());
        let (row_in, col_in) = if flag("dt2D") {
            (cell("r1"), cell("r2"))
        } else if flag("dtr") {
            (cell("r1"), None)
        } else {
            (None, cell("r1"))
        };
        if row_in.is_none() && col_in.is_none() {
            continue;
        }
        let a1 = |a: Option<CellAddr>| a.map_or(String::new(), CellAddr::to_a1);
        let formula = format!("TABLE({},{})", a1(row_in), a1(col_in));
        for a in body.iter() {
            let c = sheet.entry(a);
            c.content = sp_model::Content::Formula(formula.clone());
        }
        let range = CellRange::new(
            CellAddr::new(body.start.row - 1, body.start.col - 1),
            body.end,
        );
        let abs1 = |a: Option<CellAddr>| a.map_or(String::new(), |a| abs_ref(CellRange::single(a)));
        if sheet.analysis.data_tables.len() < MAX_OBJECTS {
            sheet.analysis.data_tables.push(sp_model::pivot::DataTable {
                range: abs_ref(range),
                row_input: abs1(row_in),
                col_input: abs1(col_in),
            });
        }
    }
}

/// How a `TABLE()` cell is written: the body's top-left cell carries the
/// `<f t="dataTable">` record, the other body cells only their values (`Some("")`).
/// `None` for anything else (a normal formula).
pub fn data_table_f(sheet: &sp_model::Sheet, at: CellAddr, formula: &str) -> Option<String> {
    if !sp_formula::whatif::is_table_formula(formula) {
        return None;
    }
    let dt = sheet.analysis.data_tables.iter().find_map(|d| {
        let r = CellRange::parse(&d.range.replace('$', "")).ok()?;
        if r.rows() < 2 || r.cols() < 2 {
            return None;
        }
        let body = CellRange::new(CellAddr::new(r.start.row + 1, r.start.col + 1), r.end);
        body.contains(at).then_some((d, body))
    })?;
    let (d, body) = dt;
    if at != body.start {
        return Some(String::new());
    }
    let plain = |t: &str| t.replace('$', "");
    let (row_in, col_in) = (plain(&d.row_input), plain(&d.col_input));
    let attrs = match (row_in.is_empty(), col_in.is_empty()) {
        (false, false) => format!(r#" dt2D="1" dtr="1" r1="{row_in}" r2="{col_in}""#),
        (false, true) => format!(r#" dt2D="0" dtr="1" r1="{row_in}""#),
        (true, false) => format!(r#" dt2D="0" dtr="0" r1="{col_in}""#),
        (true, true) => return None,
    };
    Some(format!(
        r#"<f t="dataTable" ref="{}"{attrs}/>"#,
        body.to_a1()
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use sp_model::Cell;

    fn book() -> Workbook {
        let mut wb = Workbook::new();
        wb.sheets[0].name = "Data Sheet".into();
        let rows = [
            ["Region", "Product", "Sales"],
            ["East", "A", "10"],
            ["East", "B", "20"],
            ["West", "A", "30"],
            ["West", "", "40"],
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                if v.is_empty() {
                    continue;
                }
                let val = v.parse::<f64>().map_or(Value::text(*v), Value::Number);
                wb.sheets[0].set(CellAddr::new(r as u32, c as u32), Cell::constant(val));
            }
        }
        let block: Vec<Vec<Value>> = (0..5)
            .map(|r| {
                (0..3)
                    .map(|c| wb.sheets[0].value(CellAddr::new(r, c)).clone())
                    .collect()
            })
            .collect();
        let cache = sp_formula::pivot::build_cache(&block).unwrap();
        wb.sheets.push(sp_model::Sheet::new("Report"));
        wb.sheets[1].analysis.pivots.push(PivotTable {
            name: "Sales Pivot".into(),
            source: "'Data Sheet'!$A$1:$C$5".into(),
            location: "$B$2".into(),
            cache,
            rows: vec![0],
            cols: vec![1],
            filters: vec![],
            values: vec![
                DataField {
                    field: 2,
                    func: Aggregate::Sum,
                    ..DataField::default()
                },
                DataField {
                    field: 2,
                    func: Aggregate::Max,
                    show_as: ShowAs::PercentOfTotal,
                    name: "Top".into(),
                },
            ],
            layout: PivotLayout::Tabular,
            grand_total_row: false,
            field_filters: vec![FieldFilter {
                field: 0,
                hidden: vec![Value::text("West")],
            }],
            ..PivotTable::default()
        });
        wb
    }

    fn round_trip(wb: &Workbook) -> (Workbook, Vec<u8>) {
        let mut buf = std::io::Cursor::new(Vec::new());
        crate::xlsx_write::write_xlsx(wb, &mut buf).unwrap();
        let bytes = buf.into_inner();
        let problems = crate::xlsx_passthrough::validate_package(&bytes);
        assert!(problems.is_empty(), "{problems:?}");
        let (back, _) = crate::xlsx_read::read_xlsx(std::io::Cursor::new(bytes.clone())).unwrap();
        (back, bytes)
    }

    fn same_definition(a: &PivotTable, b: &PivotTable) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.source, b.source);
        assert_eq!(a.location, b.location);
        assert_eq!(a.cache.fields, b.cache.fields);
        assert_eq!(a.cache.records, b.cache.records);
        assert_eq!(a.rows, b.rows);
        assert_eq!(a.cols, b.cols);
        assert_eq!(a.filters, b.filters);
        assert_eq!(a.values, b.values);
        assert_eq!(a.layout, b.layout);
        assert_eq!(a.grand_total_row, b.grand_total_row);
        assert_eq!(a.grand_total_col, b.grand_total_col);
        assert_eq!(a.subtotals, b.subtotals);
        assert_eq!(a.field_filters, b.field_filters);
    }

    #[test]
    fn generated_pivot_round_trips_and_then_stays_verbatim() {
        let wb = book();
        let (back, bytes) = round_trip(&wb);
        let p = &back.sheets[1].analysis.pivots[0];
        same_definition(&wb.sheets[1].analysis.pivots[0], p);
        assert!(p.is_pristine());
        assert!(
            back.passthrough
                .parts
                .iter()
                .all(|x| !x.path.contains("pivot"))
        );
        // A second save writes the loaded parts back byte for byte.
        let (again, bytes2) = round_trip(&back);
        same_definition(p, &again.sheets[1].analysis.pivots[0]);
        let part = |b: &[u8], n: &str| {
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(b.to_vec())).unwrap();
            let mut s = Vec::new();
            std::io::Read::read_to_end(&mut z.by_name(n).unwrap(), &mut s).unwrap();
            s
        };
        for n in [
            "xl/pivotTables/pivotTable1.xml",
            "xl/pivotCache/pivotCacheDefinition1.xml",
            "xl/pivotCache/pivotCacheRecords1.xml",
        ] {
            assert_eq!(part(&bytes, n), part(&bytes2, n), "{n}");
        }
        let wbx = String::from_utf8(part(&bytes2, "xl/workbook.xml")).unwrap();
        assert!(wbx.contains(r#"<pivotCache cacheId="1""#), "{wbx}");
    }

    #[test]
    fn page_field_with_one_item_and_table_source() {
        let mut wb = book();
        {
            let p = &mut wb.sheets[1].analysis.pivots[0];
            p.cols.clear();
            p.filters = vec![1];
            p.layout = PivotLayout::Compact;
            p.subtotals = false;
            p.values.truncate(1);
            p.field_filters = vec![FieldFilter {
                field: 1,
                hidden: vec![Value::text("B"), Value::Empty],
            }];
            p.location = "$A$1".into();
        }
        let (back, _) = round_trip(&wb);
        let p = &back.sheets[1].analysis.pivots[0];
        same_definition(&wb.sheets[1].analysis.pivots[0], p);
    }

    #[test]
    fn unmodelled_pivots_stay_passed_through() {
        let def = CacheDef {
            source: "S!$A$1:$B$2".into(),
            fields: vec!["a".into(), "b".into()],
            shared: vec![vec![Value::text("x")], vec![]],
        };
        let ok = r#"<pivotTableDefinition name="P" cacheId="3"><location ref="A3:B5"/><pivotFields count="2"><pivotField axis="axisRow"><items count="2"><item x="0"/><item t="default"/></items></pivotField><pivotField dataField="1"/></pivotFields><rowFields count="1"><field x="0"/></rowFields><dataFields count="1"><dataField name="Sum of b" fld="1"/></dataFields></pivotTableDefinition>"#;
        let recs = vec![vec![Value::text("x"), Value::Number(1.0)]];
        let (p, id) = parse_table(ok, &def, recs.clone()).unwrap();
        assert_eq!(id, 3);
        assert_eq!(p.values[0].name, "");
        assert_eq!(p.output, "$A$3:$B$5");
        for bad in [
            ok.replace(
                "</pivotTableDefinition>",
                "<filters/></pivotTableDefinition>",
            ),
            ok.replace(
                r#"dataField name="Sum of b" fld="1""#,
                r#"dataField name="x" fld="1" showDataAs="difference""#,
            ),
            ok.replace(
                r#"<field x="0"/></rowFields>"#,
                r#"<field x="-2"/></rowFields>"#,
            ),
            ok.replace(r#"<item t="default"/>"#, r#"<item t="sum"/>"#),
            ok.replace(r#"axis="axisRow""#, r#"axis="axisRow" sumSubtotal="1""#),
        ] {
            assert!(parse_table(&bad, &def, recs.clone()).is_none(), "{bad}");
        }
        let cache = r#"<pivotCacheDefinition><cacheSource type="worksheet"><worksheetSource ref="A1:B3" sheet="My Data"/></cacheSource><cacheFields count="2"><cacheField name="a"><sharedItems><s v="x"/><d v="2024-01-02T12:00:00"/></sharedItems></cacheField><cacheField name="b"><sharedItems/></cacheField></cacheFields></pivotCacheDefinition>"#;
        let d = parse_cache(cache, "Home").unwrap();
        assert_eq!(d.source, "'My Data'!$A$1:$B$3");
        assert_eq!(d.shared[0][1], Value::Number(45293.5));
        assert!(
            parse_cache(
                &cache.replace(
                    "<cacheField name=\"b\">",
                    "<cacheField name=\"b\" formula=\"a*2\">"
                ),
                "H"
            )
            .is_none()
        );
        assert!(
            parse_cache(
                &cache.replace("type=\"worksheet\"", "type=\"external\""),
                "H"
            )
            .is_none()
        );
        let rec = r#"<pivotCacheRecords count="2"><r><x v="1"/><n v="5"/></r><r><x v="0"/><m/></r></pivotCacheRecords>"#;
        let r = parse_records(rec, &d).unwrap();
        assert_eq!(
            r,
            vec![
                vec![Value::Number(45293.5), Value::Number(5.0)],
                vec![Value::text("x"), Value::Empty]
            ]
        );
        assert!(
            parse_records(
                r#"<pivotCacheRecords><r><x v="9"/><m/></r></pivotCacheRecords>"#,
                &d
            )
            .is_none()
        );
        assert!(
            parse_records(r#"<pivotCacheRecords><r><m/></r></pivotCacheRecords>"#, &d).is_none()
        );
    }

    #[test]
    fn corpus_pivot_is_adopted_and_refreshable() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/files/xlsx/tests/example.xlsx");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("corpus not fetched; skipping");
            return;
        };
        let (wb, _) = crate::xlsx_read::read_xlsx(std::io::Cursor::new(bytes)).unwrap();
        let pivots: Vec<&PivotTable> = wb
            .sheets
            .iter()
            .flat_map(|s| s.analysis.pivots.iter())
            .collect();
        assert_eq!(pivots.len(), 1);
        let p = pivots[0];
        assert_eq!(p.source, "Table1");
        assert_eq!(p.rows.len(), 3);
        assert_eq!(p.values[0].field, 1);
        assert_eq!(p.output, "$A$3:$B$13");
        assert!(p.is_pristine());
        // The layout engine reproduces the saved report's size.
        let out = sp_formula::pivot::layout(p).unwrap();
        assert_eq!((out.rows, out.cols), (11, 2));
        assert!(
            wb.passthrough
                .parts
                .iter()
                .all(|x| !x.path.contains("pivot"))
        );
        assert!(
            wb.passthrough
                .elements
                .iter()
                .all(|e| e.name != "pivotCaches")
        );
        // Edited: regenerated (new definition, same cache data), still a valid package.
        let mut wb = wb;
        let (si, _) = wb
            .sheets
            .iter()
            .enumerate()
            .find(|(_, s)| !s.analysis.pivots.is_empty())
            .unwrap();
        wb.sheets[si].analysis.pivots[0].subtotals = false;
        wb.sheets[si].analysis.pivots[0].grand_total_row = false;
        let (back, _) = round_trip(&wb);
        let q = &back.sheets[si].analysis.pivots[0];
        same_definition(&wb.sheets[si].analysis.pivots[0], q);
    }

    #[test]
    fn data_tables_round_trip() {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        let at = |a: &str| CellAddr::parse(a).unwrap();
        s.set(at("A1"), Cell::constant(Value::Number(2.0)));
        s.set(at("A2"), Cell::constant(Value::Number(3.0)));
        s.set(at("C3"), Cell::formula("A1*A2"));
        for a in ["D3", "E3", "C4", "C5"] {
            s.set(at(a), Cell::constant(Value::Number(1.0)));
        }
        for a in ["D4", "E4", "D5", "E5"] {
            s.set(at(a), Cell::formula("TABLE(A1,A2)"));
        }
        s.analysis.data_tables.push(sp_model::pivot::DataTable {
            range: "$C$3:$E$5".into(),
            row_input: "$A$1".into(),
            col_input: "$A$2".into(),
        });
        s.set(at("H2"), Cell::formula("A1+1"));
        s.set(at("G3"), Cell::constant(Value::Number(9.0)));
        s.set(at("H3"), Cell::formula("TABLE(,A1)"));
        s.set(at("J9"), Cell::formula("TABLE(,A1)"));
        s.analysis.data_tables.push(sp_model::pivot::DataTable {
            range: "$G$2:$H$3".into(),
            row_input: String::new(),
            col_input: "$A$1".into(),
        });
        sp_formula::recalc(
            &mut wb,
            &mut sp_formula::AstCache::default(),
            Default::default(),
        );
        let (back, bytes) = round_trip(&wb);
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut x = String::new();
        std::io::Read::read_to_string(&mut z.by_name("xl/worksheets/sheet1.xml").unwrap(), &mut x)
            .unwrap();
        assert!(
            x.contains(r#"<f t="dataTable" ref="D4:E5" dt2D="1" dtr="1" r1="A1" r2="A2"/>"#),
            "{x}"
        );
        assert!(
            x.contains(r#"<f t="dataTable" ref="H3" dt2D="0" dtr="0" r1="A1"/><v>10</v>"#),
            "{x}"
        );
        assert!(x.contains(r#"<c r="E5"><v>"#), "{x}");
        let b = &back.sheets[0];
        assert_eq!(b.get(at("E5")).unwrap().formula_src(), Some("TABLE(A1,A2)"));
        assert_eq!(b.value(at("E5")), &Value::Number(1.0));
        let mut got = b.analysis.data_tables.clone();
        got.sort_by(|x, y| x.range.cmp(&y.range));
        assert_eq!(got, wb.sheets[0].analysis.data_tables);
        assert_eq!(b.get(at("H3")).unwrap().formula_src(), Some("TABLE(,A1)"));
        // A TABLE cell outside any data table is a plain formula.
        assert_eq!(data_table_f(b, at("J9"), "TABLE(,A1)"), None);
        assert_eq!(data_table_f(b, at("E5"), "SUM(1)"), None);
        assert_eq!(
            data_table_f(b, at("E5"), "TABLE(A1,A2)"),
            Some(String::new())
        );
    }

    #[test]
    fn captured_caches_are_merged() {
        let mut plan = PivotPlan::new(&book());
        plan.captured_caches = r#"<pivotCache cacheId="1" r:id="rId9"/>"#.into();
        let valid: HashSet<String> = ["rId9".to_string()].into();
        let mut n = 20;
        let mut rels = Vec::new();
        let (x, keep) = plan
            .workbook_element(
                &valid,
                &mut || {
                    n += 1;
                    format!("rId{n}")
                },
                &mut rels,
            )
            .unwrap();
        assert!(
            x.starts_with(r#"<pivotCaches><pivotCache cacheId="1" r:id="rId9"/><pivotCache"#),
            "{x}"
        );
        assert!(keep.contains("rId9"));
        assert_eq!(rels.len(), 1);
        // Dangling captured ids are dropped.
        let (x, keep) = plan
            .workbook_element(&HashSet::new(), &mut || "rId30".into(), &mut Vec::new())
            .unwrap();
        assert!(!x.contains("rId9") && keep.is_empty());
    }
}
