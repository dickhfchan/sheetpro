//! Charts and sparklines: floating chart objects anchored to cells, and in-cell sparkline
//! groups. Pure data; series refer to cells through formula text (e.g. `Sheet1!$B$2:$B$6`),
//! resolved by the engine when drawing or saving.

use crate::Color;
use serde::{Deserialize, Serialize};
use sp_core::CellAddr;

/// Pixels (at 100% zoom, 96 dpi) per English Metric Unit.
pub const EMU_PER_PX: f64 = 9525.0;

/// Where a floating object sits: two cell corners plus pixel offsets inside them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Anchor {
    pub from: CellAddr,
    /// Offset of the top-left corner inside `from`, px at 100% zoom (x, y).
    pub from_off: (f64, f64),
    pub to: CellAddr,
    /// Offset of the bottom-right corner inside `to`, px at 100% zoom.
    pub to_off: (f64, f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ChartKind {
    #[default]
    Column,
    Bar,
    Line,
    Pie,
    Doughnut,
    Area,
    Scatter,
    Histogram,
    /// Column series plus line series (per-series `kind`), optionally on a secondary axis.
    Combo,
}

impl ChartKind {
    pub fn name(self) -> &'static str {
        match self {
            ChartKind::Column => "column",
            ChartKind::Bar => "bar",
            ChartKind::Line => "line",
            ChartKind::Pie => "pie",
            ChartKind::Doughnut => "doughnut",
            ChartKind::Area => "area",
            ChartKind::Scatter => "scatter",
            ChartKind::Histogram => "histogram",
            ChartKind::Combo => "combo",
        }
    }

    pub fn from_name(s: &str) -> Option<ChartKind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "column" => ChartKind::Column,
            "bar" => ChartKind::Bar,
            "line" => ChartKind::Line,
            "pie" => ChartKind::Pie,
            "doughnut" => ChartKind::Doughnut,
            "area" => ChartKind::Area,
            "scatter" | "xy" => ChartKind::Scatter,
            "histogram" => ChartKind::Histogram,
            "combo" => ChartKind::Combo,
            _ => return None,
        })
    }

    /// Kinds drawn without axes.
    pub fn is_round(self) -> bool {
        matches!(self, ChartKind::Pie | ChartKind::Doughnut)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Grouping {
    #[default]
    Clustered,
    Stacked,
    /// 100% stacked.
    PercentStacked,
}

impl Grouping {
    pub fn from_name(s: &str) -> Option<Grouping> {
        Some(match s.to_ascii_lowercase().as_str() {
            "clustered" | "standard" => Grouping::Clustered,
            "stacked" => Grouping::Stacked,
            "percent" | "percentstacked" | "100" | "100%" => Grouping::PercentStacked,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Grouping::Clustered => "clustered",
            Grouping::Stacked => "stacked",
            Grouping::PercentStacked => "percent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LegendPos {
    None,
    #[default]
    Right,
    Bottom,
    Top,
    Left,
}

impl LegendPos {
    pub fn from_name(s: &str) -> Option<LegendPos> {
        Some(match s.to_ascii_lowercase().as_str() {
            "none" | "off" | "false" => LegendPos::None,
            "right" | "r" => LegendPos::Right,
            "bottom" | "b" => LegendPos::Bottom,
            "top" | "t" => LegendPos::Top,
            "left" | "l" => LegendPos::Left,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            LegendPos::None => "none",
            LegendPos::Right => "right",
            LegendPos::Bottom => "bottom",
            LegendPos::Top => "top",
            LegendPos::Left => "left",
        }
    }
}

/// How one series is drawn (combo charts mix them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SeriesKind {
    #[default]
    Column,
    Line,
    Area,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Series {
    /// Series name: a reference (`Sheet1!$B$1`) or literal text.
    pub name: String,
    /// Category labels (or X values for scatter) reference; empty = 1, 2, 3…
    pub categories: String,
    /// Values reference.
    pub values: String,
    /// Drawing style inside a combo chart.
    #[serde(default)]
    pub kind: SeriesKind,
    /// Plotted against the secondary value axis.
    #[serde(default)]
    pub secondary: bool,
    /// Explicit colour; `None` = next theme palette colour.
    #[serde(default)]
    pub color: Option<Color>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Chart {
    pub name: String,
    pub kind: ChartKind,
    pub grouping: Grouping,
    pub anchor: Anchor,
    pub series: Vec<Series>,
    pub title: Option<String>,
    #[serde(default)]
    pub x_title: Option<String>,
    #[serde(default)]
    pub y_title: Option<String>,
    /// Italic chart / horizontal-axis / vertical-axis title.
    #[serde(default)]
    pub title_italic: bool,
    #[serde(default)]
    pub x_title_italic: bool,
    #[serde(default)]
    pub y_title_italic: bool,
    pub legend: LegendPos,
    #[serde(default)]
    pub data_labels: bool,
    /// Line charts: draw point markers.
    #[serde(default)]
    pub markers: bool,
    /// Histogram: explicit number of bins (`None` = automatic).
    #[serde(default)]
    pub bins: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SparklineKind {
    #[default]
    Line,
    Column,
    WinLoss,
}

impl SparklineKind {
    pub fn name(self) -> &'static str {
        match self {
            SparklineKind::Line => "line",
            SparklineKind::Column => "column",
            SparklineKind::WinLoss => "stacked",
        }
    }
}

/// One sparkline: a cell showing a tiny chart of a one-dimensional data range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Sparkline {
    pub location: CellAddr,
    /// Data reference, e.g. `Sheet1!B2:F2`.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SparklineGroup {
    pub kind: SparklineKind,
    pub items: Vec<Sparkline>,
    pub color: Option<Color>,
    pub negative_color: Option<Color>,
    pub markers: bool,
    pub high: bool,
    pub low: bool,
    pub first: bool,
    pub last: bool,
    pub negative: bool,
    pub marker_color: Option<Color>,
    pub high_color: Option<Color>,
    pub low_color: Option<Color>,
}

/// Every floating chart and sparkline group on a sheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Drawings {
    pub charts: Vec<Chart>,
    pub sparklines: Vec<SparklineGroup>,
}

impl Drawings {
    pub fn is_empty(&self) -> bool {
        self.charts.is_empty() && self.sparklines.is_empty()
    }

    /// The sparkline drawn in `a`, with its group.
    pub fn sparkline_at(&self, a: CellAddr) -> Option<(&SparklineGroup, &Sparkline)> {
        self.sparklines
            .iter()
            .find_map(|g| g.items.iter().find(|s| s.location == a).map(|s| (g, s)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for k in [
            ChartKind::Column,
            ChartKind::Bar,
            ChartKind::Line,
            ChartKind::Pie,
            ChartKind::Doughnut,
            ChartKind::Area,
            ChartKind::Scatter,
            ChartKind::Histogram,
            ChartKind::Combo,
        ] {
            assert_eq!(ChartKind::from_name(k.name()), Some(k));
        }
        assert_eq!(LegendPos::from_name("bottom"), Some(LegendPos::Bottom));
        assert_eq!(Grouping::from_name("100%"), Some(Grouping::PercentStacked));
    }

    #[test]
    fn sparkline_lookup() {
        let mut d = Drawings::default();
        assert!(d.is_empty());
        d.sparklines.push(SparklineGroup {
            items: vec![Sparkline {
                location: CellAddr::new(1, 5),
                data: "A2:E2".into(),
            }],
            ..SparklineGroup::default()
        });
        assert!(d.sparkline_at(CellAddr::new(1, 5)).is_some());
        assert!(d.sparkline_at(CellAddr::new(2, 5)).is_none());
    }
}
