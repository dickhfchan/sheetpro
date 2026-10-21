//! Format Chart: chart type, title, legend position and data labels for a selected chart
//! (opened by double-clicking it). OK runs `insert.chart.<type>` with `{"chart": N, …}`.

use super::{Flow, enter_pressed, error_line, label, ok_cancel, window};
use crate::App;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sp_engine::Session;

/// (type name, label) choices; doughnut is a pie with a hole.
pub const CHART_TYPES: [(&str, &str); 9] = [
    ("column", "Column"),
    ("bar", "Bar"),
    ("line", "Line"),
    ("area", "Area"),
    ("pie", "Pie"),
    ("doughnut", "Doughnut"),
    ("scatter", "Scatter"),
    ("histogram", "Histogram"),
    ("combo", "Combo"),
];

pub const LEGENDS: [&str; 5] = ["right", "bottom", "top", "left", "none"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ChartFormat {
    pub chart: usize,
    pub kind: String,
    pub has_title: bool,
    pub title: String,
    pub title_italic: bool,
    pub legend: String,
    pub data_labels: bool,
    pub error: Option<String>,
}

impl ChartFormat {
    pub fn new(s: &Session, chart: usize) -> Option<Self> {
        let c = s.wb.sheet(s.sheet_index())?.drawings.charts.get(chart)?;
        Some(ChartFormat {
            chart,
            kind: c.kind.name().into(),
            has_title: c.title.is_some(),
            title: c.title.clone().unwrap_or_default(),
            title_italic: c.title_italic,
            legend: c.legend.name().into(),
            data_labels: c.data_labels,
            error: None,
        })
    }

    pub fn params(&self) -> (String, serde_json::Value) {
        let (id, doughnut) = match self.kind.as_str() {
            "doughnut" => ("pie", true),
            k => (k, false),
        };
        let title = if self.has_title {
            json!(self.title)
        } else {
            serde_json::Value::Null
        };
        let mut p = json!({
            "chart": self.chart,
            "title": title,
            "title_italic": self.title_italic,
            "legend": self.legend,
            "data_labels": self.data_labels,
        });
        if doughnut && let Some(o) = p.as_object_mut() {
            o.insert("doughnut".into(), json!(true));
        }
        (format!("insert.chart.{id}"), p)
    }

    pub fn ui(&mut self, ctx: &egui::Context, app: &mut App) -> Flow {
        let ((ok, cancel), esc) = window(ctx, "Format Chart", 330.0, |ui| {
            egui::Grid::new("chart-fmt")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    label(ui, "Chart type:");
                    let cur = CHART_TYPES
                        .iter()
                        .find(|(k, _)| *k == self.kind)
                        .map_or("Column", |(_, l)| *l);
                    egui::ComboBox::new("chart-kind", "")
                        .selected_text(cur)
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for (k, l) in CHART_TYPES {
                                if ui.selectable_label(self.kind == k, l).clicked() {
                                    self.kind = k.into();
                                }
                            }
                        });
                    ui.end_row();
                    label(ui, "Chart title:");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.has_title, "");
                        ui.add_enabled(
                            self.has_title,
                            egui::TextEdit::singleline(&mut self.title).desired_width(150.0),
                        );
                        ui.add_enabled_ui(self.has_title, |ui| {
                            ui.checkbox(&mut self.title_italic, "Italic");
                        });
                    });
                    ui.end_row();
                    label(ui, "Legend:");
                    egui::ComboBox::new("chart-legend", "")
                        .selected_text(&self.legend)
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for l in LEGENDS {
                                if ui.selectable_label(self.legend == l, l).clicked() {
                                    self.legend = l.into();
                                }
                            }
                        });
                    ui.end_row();
                    label(ui, "Data labels:");
                    ui.checkbox(&mut self.data_labels, "Show values");
                    ui.end_row();
                });
            error_line(ui, &self.error);
            ok_cancel(ui, "OK")
        });
        if cancel || esc {
            return Flow::Close;
        }
        if ok || enter_pressed(ctx) {
            let (id, p) = self.params();
            return match app.try_exec(&id, p) {
                Ok(_) => Flow::Close,
                Err(e) => {
                    self.error = Some(e);
                    Flow::Keep
                }
            };
        }
        Flow::Keep
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn italic_title_reaches_the_chart() {
        let mut s = Session::new();
        for (c, v) in [("A1", "x"), ("A2", "1"), ("A3", "2")] {
            s.execute("cell.set", &json!({"cell": c, "input": v}))
                .unwrap();
        }
        s.execute(
            "insert.chart.column",
            &json!({"series": [{"values": "A2:A3"}], "title": "T"}),
        )
        .unwrap();
        let mut d = ChartFormat::new(&s, 0).unwrap();
        assert!(!d.title_italic);
        d.title_italic = true;
        let (id, p) = d.params();
        s.execute(&id, &p).unwrap();
        let c = &s.wb.sheet(s.sheet_index()).unwrap().drawings.charts[0];
        assert!(c.title_italic && c.title.as_deref() == Some("T"));
        // Reopening the dialog shows it ticked.
        assert!(ChartFormat::new(&s, 0).unwrap().title_italic);
    }
}
