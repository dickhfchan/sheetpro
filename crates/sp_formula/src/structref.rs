//! Structured references to tables: `Table1[Col]`, `Table1[[#Headers],[Col]]`, `[@Col]`,
//! `Table1[#All]`, `Table1[[A]:[C]]`… Lexing of the bracket body, printing, and resolution
//! to a cell range against the workbook's tables.

use sp_core::{CellAddr, CellRange, ErrorKind};
use sp_model::{Table, Workbook};

/// Row selector of a structured reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TableArea {
    All,
    Data,
    Headers,
    Totals,
    ThisRow,
}

impl TableArea {
    pub fn keyword(self) -> &'static str {
        match self {
            TableArea::All => "#All",
            TableArea::Data => "#Data",
            TableArea::Headers => "#Headers",
            TableArea::Totals => "#Totals",
            TableArea::ThisRow => "#This Row",
        }
    }

    fn from_keyword(s: &str) -> Option<TableArea> {
        let k: String = s
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        Some(match k.as_str() {
            "#all" => TableArea::All,
            "#data" => TableArea::Data,
            "#headers" => TableArea::Headers,
            "#totals" => TableArea::Totals,
            "#thisrow" => TableArea::ThisRow,
            _ => return None,
        })
    }
}

/// `Table[...]`. `table = None` is an unqualified reference (`[@Col]`) to the table that
/// contains the formula.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StructRef {
    pub table: Option<String>,
    /// Row selectors; empty = `#Data`.
    pub areas: Vec<TableArea>,
    /// First column (None = every column).
    pub col0: Option<String>,
    /// Last column of a `[A]:[B]` span.
    pub col1: Option<String>,
}

fn err(m: &str) -> String {
    format!("bad structured reference: {m}")
}

/// Unescape a column name (`'` escapes `[ ] # '`).
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\'' {
            if let Some(n) = it.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(c, '[' | ']' | '#' | '\'') {
            out.push('\'');
        }
        out.push(c);
    }
    out
}

/// Scan a bracketed body starting at `chars[i] == '['`; returns the inner text and the index
/// after the matching `]`.
pub fn scan_brackets(chars: &[char], i: usize) -> Result<(String, usize), String> {
    let mut depth = 0usize;
    let mut j = i;
    let mut inner = String::new();
    while let Some(&c) = chars.get(j) {
        match c {
            '\'' => {
                // Escape: keep both characters verbatim for the item parser.
                if depth > 0 {
                    inner.push(c);
                    if let Some(&n) = chars.get(j + 1) {
                        inner.push(n);
                    }
                }
                j += 2;
                continue;
            }
            '[' => {
                depth += 1;
                if depth > 1 {
                    inner.push(c);
                }
                if depth > 8 {
                    return Err(err("nested too deeply"));
                }
            }
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok((inner, j + 1));
                }
                inner.push(c);
            }
            _ => inner.push(c),
        }
        j += 1;
    }
    Err(err("missing ]"))
}

/// Split `[a],[b]:[c]` into bracket items with the separators between them.
fn items(s: &str) -> Result<Vec<(String, char)>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut sep = ',';
    while i < chars.len() {
        match chars.get(i) {
            Some(' ') => i += 1,
            Some('[') => {
                let (inner, n) = scan_brackets(&chars, i)?;
                out.push((inner, sep));
                i = n;
                sep = ' ';
            }
            Some(',') => {
                sep = ',';
                i += 1;
            }
            Some(':') => {
                sep = ':';
                i += 1;
            }
            _ => return Err(err("expected [")),
        }
    }
    Ok(out)
}

/// Parse the body between the outer brackets.
pub fn parse_body(table: Option<String>, body: &str) -> Result<StructRef, String> {
    let mut r = StructRef {
        table,
        areas: Vec::new(),
        col0: None,
        col1: None,
    };
    let body = body.trim();
    if body.is_empty() {
        return Ok(r);
    }
    let (body, this_row) = match body.strip_prefix('@') {
        Some(rest) => (rest.trim(), true),
        None => (body, false),
    };
    if this_row {
        r.areas.push(TableArea::ThisRow);
        if body.is_empty() {
            return Ok(r);
        }
    }
    if !body.starts_with('[') {
        // A single keyword or column name.
        if let Some(a) = body.starts_with('#').then(|| TableArea::from_keyword(body)) {
            r.areas.push(a.ok_or_else(|| err("unknown #keyword"))?);
        } else {
            r.col0 = Some(unescape(body));
        }
        return Ok(r);
    }
    for (item, sep) in items(body)? {
        let item = item.trim();
        if item.starts_with('#') {
            if sep == ':' {
                return Err(err("keyword in a column span"));
            }
            let a = TableArea::from_keyword(item).ok_or_else(|| err("unknown #keyword"))?;
            if !r.areas.contains(&a) {
                r.areas.push(a);
            }
        } else if sep == ':' {
            if r.col0.is_none() || r.col1.is_some() {
                return Err(err("bad column span"));
            }
            r.col1 = Some(unescape(item));
        } else {
            if r.col0.is_some() {
                return Err(err("more than one column"));
            }
            r.col0 = Some(unescape(item));
        }
    }
    Ok(r)
}

/// Formula text of a structured reference.
pub fn print(r: &StructRef) -> String {
    let mut s = r.table.clone().unwrap_or_default();
    let col = |c: &str| format!("[{}]", escape(c));
    let cols = match (&r.col0, &r.col1) {
        (Some(a), Some(b)) => Some(format!("{}:{}", col(a), col(b))),
        (Some(a), None) => Some(col(a)),
        _ => None,
    };
    let plain = |c: &str| {
        c.chars()
            .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.')
    };
    s.push('[');
    match (r.areas.as_slice(), &cols) {
        ([], None) => {}
        ([], Some(_)) if r.col1.is_none() => {
            s.push_str(&escape(r.col0.as_deref().unwrap_or_default()));
        }
        ([], Some(c)) => s.push_str(c),
        ([TableArea::ThisRow], None) => s.push('@'),
        ([TableArea::ThisRow], Some(_)) if r.col1.is_none() => {
            let c = r.col0.as_deref().unwrap_or_default();
            s.push('@');
            if plain(c) {
                s.push_str(c);
            } else {
                s.push_str(&col(c));
            }
        }
        ([a], None) => s.push_str(a.keyword()),
        (areas, c) => {
            let mut parts: Vec<String> =
                areas.iter().map(|a| format!("[{}]", a.keyword())).collect();
            if let Some(c) = c {
                parts.push(c.clone());
            }
            s.push_str(&parts.join(","));
        }
    }
    s.push(']');
    s
}

/// The table a reference names (or, unqualified, the table containing `cell`).
pub fn find_table<'w>(
    wb: &'w Workbook,
    r: &StructRef,
    sheet: usize,
    cell: CellAddr,
) -> Option<(usize, &'w Table)> {
    match &r.table {
        Some(n) => wb.find_table(n),
        None => wb
            .sheet(sheet)
            .and_then(|s| s.table_at(cell))
            .map(|t| (sheet, t)),
    }
}

/// Cell range a structured reference denotes. `cell = None` resolves `#This Row` to the
/// whole data body (used for dependency tracking).
pub fn resolve(t: &Table, r: &StructRef, cell: Option<CellAddr>) -> Result<CellRange, ErrorKind> {
    let (c0, c1) = match &r.col0 {
        None => (t.range.start.col, t.range.end.col),
        Some(a) => {
            let i = t.column_index(a).ok_or(ErrorKind::Ref)?;
            let j = match &r.col1 {
                Some(b) => t.column_index(b).ok_or(ErrorKind::Ref)?,
                None => i,
            };
            let x = t.sheet_col(i.min(j)).ok_or(ErrorKind::Ref)?;
            let y = t.sheet_col(i.max(j)).ok_or(ErrorKind::Ref)?;
            (x, y)
        }
    };
    let data = t.data_range().map(|d| (d.start.row, d.end.row));
    let mut lo = u32::MAX;
    let mut hi = 0u32;
    let mut add = |span: Option<(u32, u32)>| -> Result<(), ErrorKind> {
        let (a, b) = span.ok_or(ErrorKind::Ref)?;
        lo = lo.min(a);
        hi = hi.max(b);
        Ok(())
    };
    let areas: &[TableArea] = if r.areas.is_empty() {
        &[TableArea::Data]
    } else {
        &r.areas
    };
    for a in areas {
        match a {
            TableArea::All => add(Some((t.range.start.row, t.range.end.row)))?,
            TableArea::Data => add(data)?,
            TableArea::Headers => add(t.header_range().map(|h| (h.start.row, h.start.row)))?,
            TableArea::Totals => add(t.totals_range().map(|h| (h.start.row, h.start.row)))?,
            TableArea::ThisRow => match cell {
                None => add(data)?,
                Some(c) => {
                    let (d0, d1) = data.ok_or(ErrorKind::Value)?;
                    if c.row < d0 || c.row > d1 {
                        return Err(ErrorKind::Value);
                    }
                    add(Some((c.row, c.row)))?;
                }
            },
        }
    }
    if lo > hi {
        return Err(ErrorKind::Ref);
    }
    Ok(CellRange::new(CellAddr::new(lo, c0), CellAddr::new(hi, c1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(t: Option<&str>, b: &str) -> StructRef {
        parse_body(t.map(str::to_string), b).unwrap()
    }

    #[test]
    fn parse_forms() {
        let r = p(Some("T"), "Col");
        assert_eq!((r.col0.as_deref(), r.areas.len()), (Some("Col"), 0));
        let r = p(Some("T"), "[#Headers],[Col]");
        assert_eq!(r.areas, vec![TableArea::Headers]);
        let r = p(None, "@Col");
        assert_eq!(r.areas, vec![TableArea::ThisRow]);
        let r = p(None, "@[Unit Price]");
        assert_eq!(r.col0.as_deref(), Some("Unit Price"));
        let r = p(Some("T"), "[#This Row],[A]");
        assert_eq!(r.areas, vec![TableArea::ThisRow]);
        let r = p(Some("T"), "[A]:[C]");
        assert_eq!(
            (r.col0.as_deref(), r.col1.as_deref()),
            (Some("A"), Some("C"))
        );
        let r = p(Some("T"), "#All");
        assert_eq!(r.areas, vec![TableArea::All]);
        let r = p(Some("T"), "[#Data],[#Totals],[A]:[B]");
        assert_eq!(r.areas.len(), 2);
        assert_eq!(p(Some("T"), "Odd'#Name").col0.as_deref(), Some("Odd#Name"));
        assert!(parse_body(None, "#Nope").is_err());
        assert!(parse_body(None, "[A],[B]").is_err());
        assert!(parse_body(None, "[A]:[B]:[C]").is_err());
    }

    #[test]
    fn print_forms() {
        for (t, b, want) in [
            (Some("T"), "Col", "T[Col]"),
            (Some("T"), "", "T[]"),
            (Some("T"), "#All", "T[#All]"),
            (None, "@Col", "[@Col]"),
            (None, "@[Unit Price]", "[@[Unit Price]]"),
            (Some("T"), "[#Headers],[Col]", "T[[#Headers],[Col]]"),
            (Some("T"), "[A]:[C]", "T[[A]:[C]]"),
            (Some("T"), "[#This Row],[A]", "T[@A]"),
            (
                Some("T"),
                "[#Data],[#Totals],[A]:[B]",
                "T[[#Data],[#Totals],[A]:[B]]",
            ),
        ] {
            assert_eq!(print(&p(t, b)), want);
        }
    }

    #[test]
    fn resolve_ranges() {
        let mut t = Table::new(
            1,
            "T",
            CellRange::parse("B2:D6").unwrap(),
            vec!["A".into(), "B".into(), "C".into()],
        );
        t.totals_row = true;
        let rr = |b: &str, cell: Option<&str>| {
            resolve(
                &t,
                &p(Some("T"), b),
                cell.map(|c| CellAddr::parse(c).unwrap()),
            )
            .map(|r| r.to_a1())
        };
        assert_eq!(rr("B", None), Ok("C3:C5".into()));
        assert_eq!(rr("", None), Ok("B3:D5".into()));
        assert_eq!(rr("#All", None), Ok("B2:D6".into()));
        assert_eq!(rr("[#Headers],[C]", None), Ok("D2".into()));
        assert_eq!(rr("[#Totals],[A]:[B]", None), Ok("B6:C6".into()));
        assert_eq!(rr("[#Headers],[#Data],[A]", None), Ok("B2:B5".into()));
        assert_eq!(rr("@B", Some("E4")), Ok("C4".into()));
        assert_eq!(rr("@B", Some("E2")), Err(ErrorKind::Value));
        assert_eq!(rr("@B", None), Ok("C3:C5".into()));
        assert_eq!(rr("Nope", None), Err(ErrorKind::Ref));
        t.totals_row = false;
        assert_eq!(
            resolve(&t, &p(Some("T"), "#Totals"), None),
            Err(ErrorKind::Ref)
        );
    }
}

#[cfg(test)]
mod recalc_tests {
    use crate::{AstCache, RecalcOptions, recalc};
    use sp_core::{CellAddr, CellRange, Value};
    use sp_model::{Cell, Table, Workbook};

    fn book() -> Workbook {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        for (a, v) in [
            ("A1", "Item"),
            ("B1", "Qty"),
            ("C1", "Price"),
            ("D1", "Total"),
        ] {
            s.set(CellAddr::parse(a).unwrap(), Cell::constant(Value::text(v)));
        }
        for (r, q, p) in [(1u32, 2.0, 10.0), (2, 3.0, 5.0), (3, 1.0, 7.0)] {
            s.set(CellAddr::new(r, 0), Cell::constant(Value::text("x")));
            s.set(CellAddr::new(r, 1), Cell::constant(Value::Number(q)));
            s.set(CellAddr::new(r, 2), Cell::constant(Value::Number(p)));
            s.set(CellAddr::new(r, 3), Cell::formula("[@Qty]*[@Price]"));
        }
        s.tables.push(Table::new(
            1,
            "Sales",
            CellRange::parse("A1:D4").unwrap(),
            vec!["Item".into(), "Qty".into(), "Price".into(), "Total".into()],
        ));
        s.set(
            CellAddr::parse("F1").unwrap(),
            Cell::formula("SUM(Sales[Total])"),
        );
        s.set(
            CellAddr::parse("F2").unwrap(),
            Cell::formula("Sales[[#Headers],[Price]]"),
        );
        s.set(
            CellAddr::parse("F3").unwrap(),
            Cell::formula("ROWS(Sales[#All])"),
        );
        s.set(
            CellAddr::parse("F4").unwrap(),
            Cell::formula("SUM(Sales[[Qty]:[Price]])"),
        );
        s.set(CellAddr::parse("F5").unwrap(), Cell::formula("Sales[@Qty]"));
        s.set(CellAddr::parse("F6").unwrap(), Cell::formula("Nope[Qty]"));
        wb
    }

    #[test]
    fn evaluates_and_tracks_growth() {
        let mut wb = book();
        let mut cache = AstCache::default();
        recalc(&mut wb, &mut cache, RecalcOptions::default());
        let v = |wb: &Workbook, a: &str| wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone();
        assert_eq!(v(&wb, "D2"), Value::Number(20.0));
        assert_eq!(v(&wb, "F1"), Value::Number(42.0));
        assert_eq!(v(&wb, "F2"), Value::text("Price"));
        assert_eq!(v(&wb, "F3"), Value::Number(4.0));
        assert_eq!(v(&wb, "F4"), Value::Number(28.0));
        // Row 5 is outside the table body: #VALUE!.
        assert!(matches!(v(&wb, "F5"), Value::Error(_)));
        assert!(matches!(v(&wb, "F6"), Value::Error(_)));
        // Grow the table by one row: dependents of the resized table recompute.
        let s = &mut wb.sheets[0];
        s.set(CellAddr::new(4, 1), Cell::constant(Value::Number(10.0)));
        s.set(CellAddr::new(4, 2), Cell::constant(Value::Number(1.0)));
        s.set(CellAddr::new(4, 3), Cell::formula("[@Qty]*[@Price]"));
        s.tables[0].range = CellRange::parse("A1:D5").unwrap();
        crate::recalc::recalc_dirty(
            &mut wb,
            &mut cache,
            &[(0, CellAddr::new(4, 3))],
            RecalcOptions::default(),
        );
        assert_eq!(v(&wb, "F1"), Value::Number(52.0));
        assert_eq!(v(&wb, "F5"), Value::Number(10.0));
        // An incremental edit inside the column re-dirties the SUM.
        wb.sheets[0].set(CellAddr::new(1, 1), Cell::constant(Value::Number(4.0)));
        crate::recalc::recalc_dirty(
            &mut wb,
            &mut cache,
            &[(0, CellAddr::new(1, 1))],
            RecalcOptions::default(),
        );
        assert_eq!(v(&wb, "D2"), Value::Number(40.0));
        assert_eq!(v(&wb, "F1"), Value::Number(72.0));
    }
}
