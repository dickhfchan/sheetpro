//! Conditional formatting: rules attached to ranges of a sheet. Pure data; evaluation lives
//! in `sp_render::condfmt`.

use crate::style::{Border, Color};
use serde::{Deserialize, Serialize};
use sp_core::{CellAddr, CellRange};

/// A differential format: only the attributes that are `Some` override the cell's style.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct DiffStyle {
    pub font_color: Option<Color>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub fill: Option<Color>,
    pub num_fmt: Option<String>,
    /// Applied to all four sides.
    pub border: Option<Border>,
}

impl DiffStyle {
    pub fn is_empty(&self) -> bool {
        *self == DiffStyle::default()
    }

    /// Light red fill with dark red text (the default highlight preset).
    pub fn preset(name: &str) -> DiffStyle {
        let (fill, font) = match name.to_ascii_lowercase().as_str() {
            "yellow" => (Some(Color(255, 235, 156)), Some(Color(156, 87, 0))),
            "green" => (Some(Color(198, 239, 206)), Some(Color(0, 97, 0))),
            "red_fill" | "light_red_fill" => (Some(Color(255, 199, 206)), None),
            "red_text" => (None, Some(Color(156, 0, 6))),
            "red_border" => {
                return DiffStyle {
                    border: Some(Border {
                        style: crate::style::BorderStyle::Thin,
                        color: Some(Color(192, 0, 0)),
                    }),
                    ..DiffStyle::default()
                };
            }
            _ => (Some(Color(255, 199, 206)), Some(Color(156, 0, 6))),
        };
        DiffStyle {
            font_color: font,
            fill,
            ..DiffStyle::default()
        }
    }
}

/// Comparison of a `cellIs` rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CfOp {
    Between,
    NotBetween,
    Equal,
    NotEqual,
    Greater,
    Less,
    GreaterEq,
    LessEq,
}

impl CfOp {
    pub const ALL: [(CfOp, &'static str); 8] = [
        (CfOp::Between, "between"),
        (CfOp::NotBetween, "notBetween"),
        (CfOp::Equal, "equal"),
        (CfOp::NotEqual, "notEqual"),
        (CfOp::Greater, "greaterThan"),
        (CfOp::Less, "lessThan"),
        (CfOp::GreaterEq, "greaterThanOrEqual"),
        (CfOp::LessEq, "lessThanOrEqual"),
    ];

    /// File-format name (`greaterThan`…).
    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(o, _)| *o == self)
            .map_or("equal", |(_, n)| n)
    }

    pub fn from_name(s: &str) -> Option<CfOp> {
        Self::ALL
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(s))
            .map(|(o, _)| *o)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextOp {
    Contains,
    NotContains,
    BeginsWith,
    EndsWith,
}

impl TextOp {
    pub const ALL: [(TextOp, &'static str); 4] = [
        (TextOp::Contains, "containsText"),
        (TextOp::NotContains, "notContainsText"),
        (TextOp::BeginsWith, "beginsWith"),
        (TextOp::EndsWith, "endsWith"),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(o, _)| *o == self)
            .map_or("containsText", |(_, n)| n)
    }

    pub fn from_name(s: &str) -> Option<TextOp> {
        Self::ALL
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(s))
            .map(|(o, _)| *o)
    }
}

/// "A date occurring…" periods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimePeriod {
    Yesterday,
    Today,
    Tomorrow,
    Last7Days,
    LastWeek,
    ThisWeek,
    NextWeek,
    LastMonth,
    ThisMonth,
    NextMonth,
}

impl TimePeriod {
    pub const ALL: [(TimePeriod, &'static str); 10] = [
        (TimePeriod::Yesterday, "yesterday"),
        (TimePeriod::Today, "today"),
        (TimePeriod::Tomorrow, "tomorrow"),
        (TimePeriod::Last7Days, "last7Days"),
        (TimePeriod::LastWeek, "lastWeek"),
        (TimePeriod::ThisWeek, "thisWeek"),
        (TimePeriod::NextWeek, "nextWeek"),
        (TimePeriod::LastMonth, "lastMonth"),
        (TimePeriod::ThisMonth, "thisMonth"),
        (TimePeriod::NextMonth, "nextMonth"),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(o, _)| *o == self)
            .map_or("today", |(_, n)| n)
    }

    pub fn from_name(s: &str) -> Option<TimePeriod> {
        Self::ALL
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(s))
            .map(|(o, _)| *o)
    }
}

/// How a threshold of a data bar / colour scale / icon set is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CfvoKind {
    Min,
    Max,
    Num,
    Percent,
    Percentile,
    Formula,
    AutoMin,
    AutoMax,
}

impl CfvoKind {
    pub const ALL: [(CfvoKind, &'static str); 8] = [
        (CfvoKind::Min, "min"),
        (CfvoKind::Max, "max"),
        (CfvoKind::Num, "num"),
        (CfvoKind::Percent, "percent"),
        (CfvoKind::Percentile, "percentile"),
        (CfvoKind::Formula, "formula"),
        (CfvoKind::AutoMin, "autoMin"),
        (CfvoKind::AutoMax, "autoMax"),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(o, _)| *o == self)
            .map_or("min", |(_, n)| n)
    }

    pub fn from_name(s: &str) -> Option<CfvoKind> {
        Self::ALL
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(s))
            .map(|(o, _)| *o)
    }
}

/// One threshold (`cfvo`): kind plus value text (a number or a formula).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cfvo {
    pub kind: CfvoKind,
    pub value: String,
    /// Icon sets: `>=` (true) or `>` (false).
    #[serde(default = "yes")]
    pub gte: bool,
}

fn yes() -> bool {
    true
}

impl Cfvo {
    pub fn new(kind: CfvoKind, value: impl Into<String>) -> Self {
        Cfvo {
            kind,
            value: value.into(),
            gte: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CfKind {
    /// Compare the cell's value with one or two formulas.
    CellIs {
        op: CfOp,
        f1: String,
        f2: String,
    },
    Text {
        op: TextOp,
        text: String,
    },
    TimePeriod(TimePeriod),
    Duplicates,
    Unique,
    Top {
        bottom: bool,
        percent: bool,
        rank: u32,
    },
    Average {
        below: bool,
        equal: bool,
        /// Standard deviations away from the mean (0 = plain above/below).
        std_dev: u32,
    },
    Blanks,
    NoBlanks,
    Errors,
    NoErrors,
    /// Formula relative to the top-left cell of the first range; true → format.
    Expression(String),
    DataBar {
        min: Cfvo,
        max: Cfvo,
        color: Color,
        gradient: bool,
        show_value: bool,
    },
    /// Two or three thresholds with their colours.
    ColorScale {
        points: Vec<(Cfvo, Color)>,
    },
    IconSet {
        /// File-format set name, e.g. `3Arrows`, `4Rating`, `5Quarters`.
        set: String,
        /// One threshold per icon (the first is ignored: lowest bucket).
        points: Vec<Cfvo>,
        reverse: bool,
        show_value: bool,
    },
}

impl CfKind {
    /// Number of icons in a set name (`3Arrows` → 3).
    pub fn icon_count(set: &str) -> usize {
        match set.chars().next() {
            Some('4') => 4,
            Some('5') => 5,
            _ => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CfRule {
    pub kind: CfKind,
    #[serde(default)]
    pub style: DiffStyle,
    /// Lower = evaluated first. Unique within a sheet.
    pub priority: u32,
    #[serde(default)]
    pub stop_if_true: bool,
}

/// Rules sharing a set of ranges (one `conditionalFormatting` element).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CondFormat {
    pub ranges: Vec<CellRange>,
    pub rules: Vec<CfRule>,
}

impl CondFormat {
    pub fn covers(&self, a: CellAddr) -> bool {
        self.ranges.iter().any(|r| r.contains(a))
    }

    /// Top-left of the first range: formulas are written relative to it.
    pub fn anchor(&self) -> CellAddr {
        self.ranges.first().map(|r| r.start).unwrap_or_default()
    }
}

/// Next free priority on a sheet's list of formats.
pub fn next_priority(list: &[CondFormat]) -> u32 {
    list.iter()
        .flat_map(|c| c.rules.iter().map(|r| r.priority))
        .max()
        .map_or(1, |p| p.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for (o, n) in CfOp::ALL {
            assert_eq!(CfOp::from_name(n), Some(o));
            assert_eq!(o.name(), n);
        }
        assert_eq!(
            TimePeriod::from_name("last7Days"),
            Some(TimePeriod::Last7Days)
        );
        assert_eq!(CfKind::icon_count("5Quarters"), 5);
        assert_eq!(CfKind::icon_count("x"), 3);
        assert!(DiffStyle::default().is_empty());
        assert!(!DiffStyle::preset("red").is_empty());
    }

    #[test]
    fn priorities() {
        assert_eq!(next_priority(&[]), 1);
        let cf = CondFormat {
            ranges: vec![CellRange::single(CellAddr::new(1, 1))],
            rules: vec![CfRule {
                kind: CfKind::Blanks,
                style: DiffStyle::default(),
                priority: 4,
                stop_if_true: false,
            }],
        };
        assert_eq!(next_priority(std::slice::from_ref(&cf)), 5);
        assert!(cf.covers(CellAddr::new(1, 1)));
        assert_eq!(cf.anchor(), CellAddr::new(1, 1));
    }
}
