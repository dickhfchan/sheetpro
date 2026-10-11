//! Sheet/workbook features beyond cells: AutoFilter, outline, comments and notes,
//! hyperlinks, protection, data validation and page setup. Pure data (serde), plus the
//! password hashes protection needs.

use serde::{Deserialize, Serialize};
use sp_core::{CellAddr, CellRange};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------------------
// AutoFilter

/// One comparison of a custom filter (`>= 10`, `begins with "ab"` as `= ab*`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterCond {
    /// `equal`, `notEqual`, `greaterThan`, `greaterThanOrEqual`, `lessThan`,
    /// `lessThanOrEqual` (the file-format operator names).
    pub op: String,
    /// Operand as typed; `*`/`?` wildcards apply to text with `equal`/`notEqual`.
    pub val: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FilterCriteria {
    /// Keep rows whose display text is one of `values` (case-insensitive), or blank when
    /// `blanks`.
    Values { values: Vec<String>, blanks: bool },
    /// One or two comparisons joined by AND (`and = true`) or OR.
    Custom { and: bool, conds: Vec<FilterCond> },
    /// Top/bottom N items (or N percent).
    Top10 { top: bool, percent: bool, val: f64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterColumn {
    /// Column offset inside the filter range.
    pub col: u32,
    pub criteria: FilterCriteria,
}

/// AutoFilter on a sheet: the header row is `range.start.row`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoFilter {
    pub range: CellRange,
    pub columns: Vec<FilterColumn>,
}

// ---------------------------------------------------------------------------------------
// Comments and notes

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CommentKind {
    /// Legacy note (yellow box).
    #[default]
    Note,
    /// Threaded comment (conversation with replies).
    Thread,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentReply {
    pub author: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub addr: CellAddr,
    pub kind: CommentKind,
    pub author: String,
    pub text: String,
    #[serde(default)]
    pub replies: Vec<CommentReply>,
    /// Notes: always shown (not just on hover).
    #[serde(default)]
    pub visible: bool,
    #[serde(default)]
    pub resolved: bool,
}

// ---------------------------------------------------------------------------------------
// Hyperlinks

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hyperlink {
    pub range: CellRange,
    /// External target (URL, `mailto:`, file path); empty for in-workbook links.
    pub target: String,
    /// In-workbook location, e.g. `Sheet2!A1` or a defined name.
    pub location: String,
    pub tooltip: String,
    pub display: String,
}

// ---------------------------------------------------------------------------------------
// Protection

/// A stored password verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasswordHash {
    /// Legacy 16-bit hash as 4 hex digits (`password="CBEB"`).
    Legacy(String),
    /// Iterated hash (`algorithmName`/`hashValue`/`saltValue`/`spinCount`), base64 fields.
    Modern {
        algorithm: String,
        hash: String,
        salt: String,
        spin: u32,
    },
}

impl PasswordHash {
    pub fn legacy(password: &str) -> PasswordHash {
        PasswordHash::Legacy(format!("{:04X}", legacy_password_hash(password)))
    }

    /// Does `password` match? Unknown algorithms never match.
    pub fn verify(&self, password: &str) -> bool {
        match self {
            PasswordHash::Legacy(h) => {
                u16::from_str_radix(h.trim(), 16).ok() == Some(legacy_password_hash(password))
            }
            PasswordHash::Modern {
                algorithm,
                hash,
                salt,
                spin,
            } => {
                if !algorithm.eq_ignore_ascii_case("SHA-512") {
                    return false;
                }
                let (Some(salt), Some(want)) = (base64_decode(salt), base64_decode(hash)) else {
                    return false;
                };
                iterated_sha512(password, &salt, (*spin).min(10_000_000)).to_vec() == want
            }
        }
    }
}

/// What a protected sheet still lets users do (file-format `sheetProtection` flags,
/// expressed as permissions).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetProtection {
    pub password: Option<PasswordHash>,
    pub select_locked: bool,
    pub select_unlocked: bool,
    pub format_cells: bool,
    pub format_columns: bool,
    pub format_rows: bool,
    pub insert_columns: bool,
    pub insert_rows: bool,
    pub insert_hyperlinks: bool,
    pub delete_columns: bool,
    pub delete_rows: bool,
    pub sort: bool,
    pub auto_filter: bool,
    pub pivot_tables: bool,
    pub objects: bool,
    pub scenarios: bool,
}

impl Default for SheetProtection {
    /// The target app's defaults: only selecting cells is allowed.
    fn default() -> Self {
        SheetProtection {
            password: None,
            select_locked: true,
            select_unlocked: true,
            format_cells: false,
            format_columns: false,
            format_rows: false,
            insert_columns: false,
            insert_rows: false,
            insert_hyperlinks: false,
            delete_columns: false,
            delete_rows: false,
            sort: false,
            auto_filter: false,
            pivot_tables: false,
            objects: false,
            scenarios: false,
        }
    }
}

/// "Allow Edit Ranges": locked cells users may edit while the sheet is protected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AllowEditRange {
    pub title: String,
    pub ranges: Vec<CellRange>,
    pub password: Option<PasswordHash>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WorkbookProtection {
    pub password: Option<PasswordHash>,
    /// Sheets cannot be added, deleted, renamed, moved, hidden or unhidden.
    pub structure: bool,
    pub windows: bool,
}

// ---------------------------------------------------------------------------------------
// Data validation

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ValidationKind {
    #[default]
    Any,
    Whole,
    Decimal,
    List,
    Date,
    Time,
    TextLength,
    Custom,
}

impl ValidationKind {
    /// File-format `type` attribute.
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationKind::Any => "none",
            ValidationKind::Whole => "whole",
            ValidationKind::Decimal => "decimal",
            ValidationKind::List => "list",
            ValidationKind::Date => "date",
            ValidationKind::Time => "time",
            ValidationKind::TextLength => "textLength",
            ValidationKind::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Option<ValidationKind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "none" | "any" => ValidationKind::Any,
            "whole" => ValidationKind::Whole,
            "decimal" => ValidationKind::Decimal,
            "list" => ValidationKind::List,
            "date" => ValidationKind::Date,
            "time" => ValidationKind::Time,
            "textlength" | "text_length" | "length" => ValidationKind::TextLength,
            "custom" => ValidationKind::Custom,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ValidationOp {
    #[default]
    Between,
    NotBetween,
    Equal,
    NotEqual,
    GreaterThan,
    LessThan,
    GreaterThanOrEqual,
    LessThanOrEqual,
}

impl ValidationOp {
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationOp::Between => "between",
            ValidationOp::NotBetween => "notBetween",
            ValidationOp::Equal => "equal",
            ValidationOp::NotEqual => "notEqual",
            ValidationOp::GreaterThan => "greaterThan",
            ValidationOp::LessThan => "lessThan",
            ValidationOp::GreaterThanOrEqual => "greaterThanOrEqual",
            ValidationOp::LessThanOrEqual => "lessThanOrEqual",
        }
    }

    pub fn parse(s: &str) -> Option<ValidationOp> {
        Some(
            match s.to_ascii_lowercase().replace(['_', ' '], "").as_str() {
                "between" => ValidationOp::Between,
                "notbetween" => ValidationOp::NotBetween,
                "equal" | "=" => ValidationOp::Equal,
                "notequal" | "<>" => ValidationOp::NotEqual,
                "greaterthan" | ">" => ValidationOp::GreaterThan,
                "lessthan" | "<" => ValidationOp::LessThan,
                "greaterthanorequal" | ">=" => ValidationOp::GreaterThanOrEqual,
                "lessthanorequal" | "<=" => ValidationOp::LessThanOrEqual,
                _ => return None,
            },
        )
    }

    /// Does `x` satisfy the operator against bounds `a` (and `b` for between)?
    pub fn test(self, x: f64, a: f64, b: f64) -> bool {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        match self {
            ValidationOp::Between => x >= lo && x <= hi,
            ValidationOp::NotBetween => x < lo || x > hi,
            ValidationOp::Equal => x == a,
            ValidationOp::NotEqual => x != a,
            ValidationOp::GreaterThan => x > a,
            ValidationOp::LessThan => x < a,
            ValidationOp::GreaterThanOrEqual => x >= a,
            ValidationOp::LessThanOrEqual => x <= a,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ErrorStyle {
    /// Invalid entries are refused.
    #[default]
    Stop,
    /// Invalid entries are allowed after a warning.
    Warning,
    Information,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DataValidation {
    pub ranges: Vec<CellRange>,
    pub kind: ValidationKind,
    pub op: ValidationOp,
    /// Formula or constant without `=` prefix handling: stored as the file stores it
    /// (no leading `=`; list constants as `"a,b,c"` including quotes).
    pub formula1: String,
    pub formula2: String,
    pub allow_blank: bool,
    /// Show the in-cell dropdown for lists.
    pub dropdown: bool,
    pub error_style: ErrorStyle,
    pub show_error: bool,
    pub error_title: String,
    pub error: String,
    pub show_prompt: bool,
    pub prompt_title: String,
    pub prompt: String,
}

impl DataValidation {
    pub fn covers(&self, a: CellAddr) -> bool {
        self.ranges.iter().any(|r| r.contains(a))
    }

    /// Top-left cell of the first range (relative formulas are written for it).
    pub fn anchor(&self) -> CellAddr {
        self.ranges.first().map(|r| r.start).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------------------
// Page setup

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Orientation {
    #[default]
    Portrait,
    Landscape,
}

/// Margins in inches.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Margins {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub header: f64,
    pub footer: f64,
}

impl Margins {
    pub const NORMAL: Margins = Margins {
        left: 0.7,
        right: 0.7,
        top: 0.75,
        bottom: 0.75,
        header: 0.3,
        footer: 0.3,
    };
    pub const WIDE: Margins = Margins {
        left: 1.0,
        right: 1.0,
        top: 1.0,
        bottom: 1.0,
        header: 0.5,
        footer: 0.5,
    };
    pub const NARROW: Margins = Margins {
        left: 0.25,
        right: 0.25,
        top: 0.75,
        bottom: 0.75,
        header: 0.3,
        footer: 0.3,
    };
}

impl Default for Margins {
    fn default() -> Self {
        Margins::NORMAL
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageSetup {
    pub margins: Margins,
    pub orientation: Orientation,
    /// File-format paper size code (1 = Letter, 5 = Legal, 9 = A4…).
    pub paper_size: u32,
    /// Print scale in percent (10–400).
    pub scale: u32,
    /// Fit to pages wide/tall (`Some(0)` = automatic); `None` = use `scale`.
    pub fit_width: Option<u32>,
    pub fit_height: Option<u32>,
    pub print_gridlines: bool,
    pub print_headings: bool,
    /// Manual page breaks: a row break sits above the row, a column break left of the col.
    pub row_breaks: BTreeSet<u32>,
    pub col_breaks: BTreeSet<u32>,
}

impl Default for PageSetup {
    fn default() -> Self {
        PageSetup {
            margins: Margins::NORMAL,
            orientation: Orientation::Portrait,
            paper_size: 1,
            scale: 100,
            fit_width: None,
            fit_height: None,
            print_gridlines: false,
            print_headings: false,
            row_breaks: BTreeSet::new(),
            col_breaks: BTreeSet::new(),
        }
    }
}

impl PageSetup {
    pub fn is_default(&self) -> bool {
        *self == PageSetup::default()
    }
}

/// Outline (grouping) state: level per row/column (1–7; absent = 0).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outline {
    pub rows: BTreeMap<u32, u8>,
    pub cols: BTreeMap<u32, u8>,
    /// Summary rows below detail (file default) / summary columns right of detail.
    pub summary_below: bool,
    pub summary_right: bool,
}

impl Default for Outline {
    fn default() -> Self {
        Outline {
            rows: BTreeMap::new(),
            cols: BTreeMap::new(),
            summary_below: true,
            summary_right: true,
        }
    }
}

impl Outline {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.cols.is_empty()
    }
}

/// Deepest outline level the target app supports.
pub const MAX_OUTLINE_LEVEL: u8 = 7;

// ---------------------------------------------------------------------------------------
// Structural shifting

/// A row/column insert (`count > 0`) or delete (`count < 0`) of `|count|` lines starting at
/// index `at`, applied to sheet features (comments, links, validations, filters, outline…).
/// Mirrors how formula references adjust: spans grow when lines are inserted strictly
/// inside them, shrink on partial deletes, and vanish (`None`) when fully deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineShift {
    /// `true` = rows, `false` = columns.
    pub rows: bool,
    pub at: u32,
    pub count: i64,
    /// Number of lines on the axis (`MAX_ROWS` / `MAX_COLS`).
    pub max: u32,
}

impl LineShift {
    /// New position of line `v`; `None` when deleted or pushed off the sheet.
    pub fn index(&self, v: u32) -> Option<u32> {
        let (v64, at) = (i64::from(v), i64::from(self.at));
        if self.count >= 0 {
            if v64 < at {
                return Some(v);
            }
            let n = v64.checked_add(self.count)?;
            return (n < i64::from(self.max)).then_some(n as u32);
        }
        let del = self.count.checked_neg()?;
        if v64 < at {
            Some(v)
        } else if v64 < at.saturating_add(del) {
            None
        } else {
            u32::try_from(v64 - del).ok()
        }
    }

    /// New bounds of the span `[lo, hi]`.
    pub fn span(&self, lo: u32, hi: u32) -> Option<(u32, u32)> {
        if self.count >= 0 {
            let lo2 = self.index(lo)?;
            let hi2 = self.index(hi).unwrap_or_else(|| self.max.saturating_sub(1));
            return Some((lo2, hi2));
        }
        let del = self.count.checked_neg()?;
        let at = i64::from(self.at);
        let end = at.saturating_add(del) - 1;
        let (l, h) = (i64::from(lo), i64::from(hi));
        if l >= at && h <= end {
            return None;
        }
        let new_lo = if l < at {
            l
        } else if l <= end {
            at
        } else {
            l - del
        };
        let new_hi = if h < at {
            h
        } else if h <= end {
            at - 1
        } else {
            h - del
        };
        if new_lo > new_hi {
            return None;
        }
        Some((u32::try_from(new_lo).ok()?, u32::try_from(new_hi).ok()?))
    }

    pub fn addr(&self, a: CellAddr) -> Option<CellAddr> {
        if self.rows {
            Some(CellAddr::new(self.index(a.row)?, a.col))
        } else {
            Some(CellAddr::new(a.row, self.index(a.col)?))
        }
    }

    pub fn range(&self, r: CellRange) -> Option<CellRange> {
        if self.rows {
            let (lo, hi) = self.span(r.start.row, r.end.row)?;
            Some(CellRange::new(
                CellAddr::new(lo, r.start.col),
                CellAddr::new(hi, r.end.col),
            ))
        } else {
            let (lo, hi) = self.span(r.start.col, r.end.col)?;
            Some(CellRange::new(
                CellAddr::new(r.start.row, lo),
                CellAddr::new(r.end.row, hi),
            ))
        }
    }

    /// Does the edit cut through `r` (insert strictly inside, or delete part of it)?
    pub fn splits(&self, r: CellRange) -> bool {
        let (lo, hi) = if self.rows {
            (r.start.row, r.end.row)
        } else {
            (r.start.col, r.end.col)
        };
        let (lo, hi, at) = (i64::from(lo), i64::from(hi), i64::from(self.at));
        if self.count >= 0 {
            return at > lo && at <= hi;
        }
        let end = at.saturating_sub(self.count) - 1;
        let overlaps = at <= hi && end >= lo;
        let covers = at <= lo && end >= hi;
        overlaps && !covers
    }

    /// Map keyed by line index (row heights, outline levels…), re-keyed; deleted keys drop.
    pub fn keys<T: Clone>(&self, m: &BTreeMap<u32, T>) -> BTreeMap<u32, T> {
        m.iter()
            .filter_map(|(k, v)| Some((self.index(*k)?, v.clone())))
            .collect()
    }

    pub fn set(&self, s: &BTreeSet<u32>) -> BTreeSet<u32> {
        s.iter().filter_map(|k| self.index(*k)).collect()
    }
}

impl AutoFilter {
    /// The filter after a row/column insert/delete; `None` when its header row or all its
    /// columns are deleted.
    pub fn shifted(&self, sh: &LineShift) -> Option<AutoFilter> {
        if sh.rows && sh.index(self.range.start.row).is_none() {
            return None;
        }
        let range = sh.range(self.range)?;
        let columns = if sh.rows {
            self.columns.clone()
        } else {
            self.columns
                .iter()
                .filter_map(|fc| {
                    let abs = self.range.start.col.checked_add(fc.col)?;
                    let col = sh.index(abs)?.checked_sub(range.start.col)?;
                    Some(FilterColumn {
                        col,
                        criteria: fc.criteria.clone(),
                    })
                })
                .collect()
        };
        Some(AutoFilter { range, columns })
    }
}

impl Outline {
    /// Re-key levels after an insert/delete. Lines inserted inside a group join it (they
    /// take the lower of the levels on either side).
    pub fn shift(&mut self, sh: &LineShift) {
        let map = if sh.rows {
            &mut self.rows
        } else {
            &mut self.cols
        };
        let before = sh
            .at
            .checked_sub(1)
            .and_then(|p| map.get(&p))
            .copied()
            .unwrap_or(0);
        let after = map.get(&sh.at).copied().unwrap_or(0);
        let mut out = sh.keys(map);
        let level = before.min(after);
        if sh.count > 0 && level > 0 {
            let n = u32::try_from(sh.count).unwrap_or(u32::MAX);
            for k in 0..n {
                match sh.at.checked_add(k) {
                    Some(line) if line < sh.max => {
                        out.insert(line, level);
                    }
                    _ => break,
                }
            }
        }
        *map = out;
    }
}

impl PageSetup {
    /// Re-key manual page breaks on the edited axis.
    pub fn shift_breaks(&mut self, sh: &LineShift) {
        if sh.rows {
            self.row_breaks = sh.set(&self.row_breaks);
        } else {
            self.col_breaks = sh.set(&self.col_breaks);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Password hashing

/// The legacy 16-bit sheet/workbook password hash (ECMA-376 Part 4, §14.7.1).
pub fn legacy_password_hash(password: &str) -> u16 {
    let bytes: Vec<u8> = password
        .chars()
        .take(255)
        .map(|c| (u32::from(c) & 0xFF) as u8)
        .collect();
    let rot = |h: u16| ((h >> 14) & 0x01) | ((h << 1) & 0x7FFF);
    let mut h: u16 = 0;
    for b in bytes.iter().rev() {
        h = rot(h) ^ u16::from(*b);
    }
    h = rot(h);
    h ^= bytes.len() as u16;
    h ^ 0xCE4B
}

/// `H0 = SHA-512(salt ‖ UTF-16LE(password))`, `Hn = SHA-512(Hn-1 ‖ LE32(n-1))`.
pub fn iterated_sha512(password: &str, salt: &[u8], spin: u32) -> [u8; 64] {
    let mut buf: Vec<u8> = salt.to_vec();
    for u in password.encode_utf16() {
        buf.extend_from_slice(&u.to_le_bytes());
    }
    let mut h = sha512(&buf);
    for i in 0..spin {
        let mut block = h.to_vec();
        block.extend_from_slice(&i.to_le_bytes());
        h = sha512(&block);
    }
    h
}

const K512: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];

/// SHA-512 (FIPS 180-4).
pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut h: [u64; 8] = [
        0x6a09e667f3bcc908,
        0xbb67ae8584caa73b,
        0x3c6ef372fe94f82b,
        0xa54ff53a5f1d36f1,
        0x510e527fade682d1,
        0x9b05688c2b3e6c1f,
        0x1f83d9abfb41bd6b,
        0x5be0cd19137e2179,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u128).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 128 != 112 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    let mut w = [0u64; 80];
    for chunk in msg.as_chunks::<128>().0 {
        for (slot, word) in w.iter_mut().zip(chunk.as_chunks::<8>().0) {
            *slot = u64::from_be_bytes(*word);
        }
        let at = |w: &[u64; 80], k: usize| w.get(k).copied().unwrap_or(0);
        for i in 16..80 {
            let x15 = at(&w, i - 15);
            let x2 = at(&w, i - 2);
            let s0 = x15.rotate_right(1) ^ x15.rotate_right(8) ^ (x15 >> 7);
            let s1 = x2.rotate_right(19) ^ x2.rotate_right(61) ^ (x2 >> 6);
            let v = at(&w, i - 16)
                .wrapping_add(s0)
                .wrapping_add(at(&w, i - 7))
                .wrapping_add(s1);
            if let Some(slot) = w.get_mut(i) {
                *slot = v;
            }
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (k, wi) in K512.iter().zip(w.iter()) {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(*wi);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 64];
    for (o, x) in out.as_chunks_mut::<8>().0.iter_mut().zip(h) {
        o.copy_from_slice(&x.to_be_bytes());
    }
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for k in 0..4 {
            if k <= chunk.len() {
                let idx = ((n >> (18 - 6 * k)) & 63) as usize;
                out.push(char::from(B64.get(idx).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = B64.iter().position(|x| *x == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn sha512_vectors() {
        assert_eq!(
            hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(hex(&sha512(b"")).get(..16), Some("cf83e1357eefb8bd"));
    }

    #[test]
    fn base64_roundtrip() {
        for s in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(base64_decode(&base64_encode(s)).unwrap(), s);
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }

    #[test]
    fn passwords() {
        assert_eq!(format!("{:04X}", legacy_password_hash("password")), "83AF");
        let h = PasswordHash::legacy("secret");
        assert!(h.verify("secret"));
        assert!(!h.verify("Secret"));
        let salt = b"0123456789abcdef";
        let m = PasswordHash::Modern {
            algorithm: "SHA-512".into(),
            hash: base64_encode(&iterated_sha512("pw", salt, 10)),
            salt: base64_encode(salt),
            spin: 10,
        };
        assert!(m.verify("pw"));
        assert!(!m.verify("px"));
    }

    #[test]
    fn line_shift() {
        let ins = LineShift {
            rows: true,
            at: 4,
            count: 2,
            max: 10,
        };
        assert_eq!(ins.index(3), Some(3));
        assert_eq!(ins.index(4), Some(6));
        assert_eq!(ins.index(8), None);
        assert_eq!(ins.span(2, 5), Some((2, 7)));
        assert!(ins.splits(CellRange::parse("A3:A6").unwrap()));
        assert!(!ins.splits(CellRange::parse("A5:A6").unwrap()));
        let del = LineShift {
            rows: true,
            at: 4,
            count: -2,
            max: 10,
        };
        assert_eq!(del.index(5), None);
        assert_eq!(del.index(7), Some(5));
        assert_eq!(del.span(4, 5), None);
        assert_eq!(del.span(5, 8), Some((4, 6)));
        assert!(del.splits(CellRange::parse("A5:A9").unwrap()));
        assert!(!del.splits(CellRange::parse("A5:A6").unwrap()));
    }

    #[test]
    fn validation_ops() {
        assert!(ValidationOp::Between.test(5.0, 10.0, 1.0));
        assert!(ValidationOp::NotBetween.test(11.0, 1.0, 10.0));
        assert_eq!(
            ValidationOp::parse(">="),
            Some(ValidationOp::GreaterThanOrEqual)
        );
        assert_eq!(
            ValidationKind::parse("textLength"),
            Some(ValidationKind::TextLength)
        );
    }
}
