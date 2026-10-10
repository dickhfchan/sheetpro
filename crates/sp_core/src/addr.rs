//! Cell addresses and ranges, A1 notation.

use crate::{CoreError, MAX_COLS, MAX_ROWS};
use serde::{Deserialize, Serialize};
use std::fmt;

/// 0-based cell address.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
pub struct CellAddr {
    pub row: u32,
    pub col: u32,
}

impl CellAddr {
    pub const fn new(row: u32, col: u32) -> Self {
        Self { row, col }
    }

    /// Parse `A1`, `$B$2`, `xfd1048576` (case-insensitive, `$` ignored).
    pub fn parse(s: &str) -> Result<Self, CoreError> {
        parse_a1_parts(s)
            .map(|(c, _, r, _)| CellAddr::new(r, c))
            .ok_or_else(|| CoreError::BadRef(s.to_string()))
    }

    pub fn to_a1(self) -> String {
        format!("{}{}", col_to_letters(self.col), u64::from(self.row) + 1)
    }

    /// Offset by signed deltas, `None` if the result leaves the sheet.
    pub fn offset(self, drow: i64, dcol: i64) -> Option<Self> {
        let r = i64::from(self.row).checked_add(drow)?;
        let c = i64::from(self.col).checked_add(dcol)?;
        if r < 0 || c < 0 || r >= i64::from(MAX_ROWS) || c >= i64::from(MAX_COLS) {
            return None;
        }
        Some(CellAddr::new(
            u32::try_from(r).ok()?,
            u32::try_from(c).ok()?,
        ))
    }
}

impl fmt::Display for CellAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_a1())
    }
}

/// Parse an A1 token into (col, col_abs, row, row_abs), all 0-based. Returns `None` on
/// anything that is not a complete in-bounds A1 reference.
pub fn parse_a1_parts(s: &str) -> Option<(u32, bool, u32, bool)> {
    let b = s.as_bytes();
    let mut i = 0;
    let col_abs = b.first() == Some(&b'$');
    if col_abs {
        i += 1;
    }
    let col_start = i;
    while b.get(i).is_some_and(u8::is_ascii_alphabetic) {
        i += 1;
    }
    let letters = s.get(col_start..i)?;
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let row_abs = b.get(i) == Some(&b'$');
    if row_abs {
        i += 1;
    }
    let digits = s.get(i..)?;
    if digits.is_empty() || digits.len() > 7 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if digits.starts_with('0') {
        return None;
    }
    let row1: u32 = digits.parse().ok()?;
    if row1 == 0 || row1 > MAX_ROWS {
        return None;
    }
    let col = letters_to_col(letters)?;
    Some((col, col_abs, row1 - 1, row_abs))
}

/// `0 → "A"`, `25 → "Z"`, `26 → "AA"`.
pub fn col_to_letters(col: u32) -> String {
    let mut n = u64::from(col) + 1;
    let mut out = Vec::with_capacity(3);
    while n > 0 {
        let rem = (n - 1) % 26;
        out.push(b'A' + rem as u8);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// `"A" → 0`, `"aa" → 26`. `None` if not letters or beyond `XFD`.
pub fn letters_to_col(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 3 {
        return None;
    }
    let mut n: u32 = 0;
    for ch in s.bytes() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        n = n
            .checked_mul(26)?
            .checked_add(u32::from(ch.to_ascii_uppercase() - b'A') + 1)?;
    }
    let col = n.checked_sub(1)?;
    (col < MAX_COLS).then_some(col)
}

/// Inclusive rectangular range, always normalised (`start <= end` component-wise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct CellRange {
    pub start: CellAddr,
    pub end: CellAddr,
}

impl CellRange {
    pub fn new(a: CellAddr, b: CellAddr) -> Self {
        Self {
            start: CellAddr::new(a.row.min(b.row), a.col.min(b.col)),
            end: CellAddr::new(a.row.max(b.row), a.col.max(b.col)),
        }
    }

    pub fn single(a: CellAddr) -> Self {
        Self { start: a, end: a }
    }

    /// Parse `A1`, `A1:B2`, `A:C` (whole columns), `2:5` (whole rows).
    pub fn parse(s: &str) -> Result<Self, CoreError> {
        let bad = || CoreError::BadRef(s.to_string());
        let s = s.trim();
        match s.split_once(':') {
            None => Ok(CellRange::single(CellAddr::parse(s)?)),
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (CellAddr::parse(a), CellAddr::parse(b)) {
                    return Ok(CellRange::new(a, b));
                }
                let strip = |t: &str| t.trim_start_matches('$').to_string();
                let (a, b) = (strip(a), strip(b));
                if let (Some(c1), Some(c2)) = (letters_to_col(&a), letters_to_col(&b)) {
                    return Ok(CellRange::new(
                        CellAddr::new(0, c1),
                        CellAddr::new(MAX_ROWS - 1, c2),
                    ));
                }
                let r1: u32 = a.parse().map_err(|_| bad())?;
                let r2: u32 = b.parse().map_err(|_| bad())?;
                if r1 == 0 || r2 == 0 || r1 > MAX_ROWS || r2 > MAX_ROWS {
                    return Err(bad());
                }
                Ok(CellRange::new(
                    CellAddr::new(r1 - 1, 0),
                    CellAddr::new(r2 - 1, MAX_COLS - 1),
                ))
            }
        }
    }

    pub fn rows(&self) -> u32 {
        self.end.row - self.start.row + 1
    }

    pub fn cols(&self) -> u32 {
        self.end.col - self.start.col + 1
    }

    pub fn cell_count(&self) -> u64 {
        u64::from(self.rows()) * u64::from(self.cols())
    }

    pub fn contains(&self, a: CellAddr) -> bool {
        a.row >= self.start.row
            && a.row <= self.end.row
            && a.col >= self.start.col
            && a.col <= self.end.col
    }

    pub fn is_single(&self) -> bool {
        self.start == self.end
    }

    pub fn is_full_rows(&self) -> bool {
        self.start.col == 0 && self.end.col == MAX_COLS - 1
    }

    pub fn is_full_cols(&self) -> bool {
        self.start.row == 0 && self.end.row == MAX_ROWS - 1
    }

    pub fn intersect(&self, o: &CellRange) -> Option<CellRange> {
        let r0 = self.start.row.max(o.start.row);
        let r1 = self.end.row.min(o.end.row);
        let c0 = self.start.col.max(o.start.col);
        let c1 = self.end.col.min(o.end.col);
        (r0 <= r1 && c0 <= c1).then(|| CellRange::new(CellAddr::new(r0, c0), CellAddr::new(r1, c1)))
    }

    /// Row-major iteration over every address.
    pub fn iter(&self) -> impl Iterator<Item = CellAddr> + '_ {
        let (s, e) = (self.start, self.end);
        (s.row..=e.row).flat_map(move |r| (s.col..=e.col).map(move |c| CellAddr::new(r, c)))
    }

    pub fn to_a1(&self) -> String {
        if self.is_single() {
            self.start.to_a1()
        } else if self.is_full_cols() {
            format!(
                "{}:{}",
                col_to_letters(self.start.col),
                col_to_letters(self.end.col)
            )
        } else if self.is_full_rows() {
            format!("{}:{}", self.start.row + 1, self.end.row + 1)
        } else {
            format!("{}:{}", self.start.to_a1(), self.end.to_a1())
        }
    }
}

impl fmt::Display for CellRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_a1())
    }
}

/// Split an optional sheet prefix: `Sheet2!A1` → (Some("Sheet2"), "A1"),
/// `'My ''Q'' Sheet'!A1` → (Some("My 'Q' Sheet"), "A1").
pub fn split_sheet_prefix(s: &str) -> (Option<String>, &str) {
    let Some(bang) = s.rfind('!') else {
        return (None, s);
    };
    let (sheet, rest) = (s.get(..bang).unwrap_or(""), s.get(bang + 1..).unwrap_or(""));
    let sheet = if sheet.len() >= 2 && sheet.starts_with('\'') && sheet.ends_with('\'') {
        sheet
            .get(1..sheet.len() - 1)
            .unwrap_or("")
            .replace("''", "'")
    } else {
        sheet.to_string()
    };
    (Some(sheet), rest)
}

/// Quote a sheet name for use in a formula if needed.
pub fn quote_sheet_name(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && parse_a1_parts(name).is_none();
    if plain {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_roundtrip() {
        for c in [0, 1, 25, 26, 27, 51, 52, 701, 702, 16_383] {
            assert_eq!(letters_to_col(&col_to_letters(c)), Some(c), "{c}");
        }
        assert_eq!(col_to_letters(16_383), "XFD");
        assert_eq!(letters_to_col("XFE"), None);
    }

    #[test]
    fn parse_a1() {
        assert_eq!(CellAddr::parse("A1").unwrap(), CellAddr::new(0, 0));
        assert_eq!(CellAddr::parse("$c$10").unwrap(), CellAddr::new(9, 2));
        assert!(CellAddr::parse("A0").is_err());
        assert!(CellAddr::parse("A1048577").is_err());
        assert!(CellAddr::parse("1A").is_err());
        assert!(CellAddr::parse("").is_err());
        assert!(CellAddr::parse("$").is_err());
    }

    #[test]
    fn ranges() {
        let r = CellRange::parse("B3:A1").unwrap();
        assert_eq!(r.to_a1(), "A1:B3");
        assert_eq!(r.cell_count(), 6);
        assert_eq!(CellRange::parse("A:C").unwrap().to_a1(), "A:C");
        assert_eq!(CellRange::parse("2:5").unwrap().to_a1(), "2:5");
        assert!(CellRange::parse("0:5").is_err());
    }

    #[test]
    fn sheet_prefix() {
        assert_eq!(
            split_sheet_prefix("'It''s'!A1"),
            (Some("It's".into()), "A1")
        );
        assert_eq!(split_sheet_prefix("A1"), (None, "A1"));
        assert_eq!(quote_sheet_name("Sheet1"), "Sheet1");
        assert_eq!(quote_sheet_name("My Sheet"), "'My Sheet'");
        assert_eq!(quote_sheet_name("A1"), "'A1'");
    }
}
