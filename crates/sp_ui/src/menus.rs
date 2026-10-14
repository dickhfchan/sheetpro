//! Ribbon controls that are more than a button (font pickers, colour palettes) and the
//! right-click menus on the grid and sheet tabs. Every item runs a command (or opens the
//! command's dialog) through `App::activate_command` / `App::exec`.

use crate::App;
use crate::dialogs::format_cells::{FONT_FAMILIES, FONT_SIZES};
use crate::dialogs::misc::{Prompt, TabColor};
use crate::dialogs::widgets::swatch_grid;
use crate::dialogs::{Dialog, misc};
use crate::theme::{LIGHT as T, hex};
use egui::{RichText, Ui};
use serde_json::json;
use sp_engine::{Session, catalog, registry};
use sp_model::{Style, StyleId};

fn active_style(s: &Session) -> Style {
    let id =
        s.wb.sheet(s.sheet_index())
            .and_then(|sh| sh.get(s.sel.active))
            .map_or(StyleId::DEFAULT, |c| c.style);
    s.wb.styles.get(id).clone()
}

fn enabled(s: &Session, id: &str) -> bool {
    registry().get(id).is_some_and(|c| (c.spec.enabled)(s))
}

impl App {
    /// One ribbon control for a catalog item.
    pub(crate) fn ribbon_item(&mut self, ui: &mut Ui, item: &catalog::CatalogItem) {
        let on = enabled(&self.session, &item.id);
        let color = if on {
            T.ribbon_text
        } else {
            T.ribbon_text_disabled
        };
        let tip = if item.key.is_empty() {
            item.notes.clone()
        } else {
            format!("{} ({})", item.notes, item.key)
        };
        match item.id.as_str() {
            "home.font.family" if on => {
                let cur = active_style(&self.session).font_name;
                egui::ComboBox::new("rb-font", "")
                    .selected_text(RichText::new(&cur).size(11.0))
                    .width(118.0)
                    .show_ui(ui, |ui| {
                        for f in FONT_FAMILIES {
                            if ui
                                .selectable_label(cur.eq_ignore_ascii_case(f), f)
                                .clicked()
                            {
                                self.exec("home.font.family", json!({ "name": f }));
                            }
                        }
                    })
                    .response
                    .on_hover_text(tip);
            }
            "home.font.size" if on => {
                let cur = active_style(&self.session).font_size() as f32;
                egui::ComboBox::new("rb-size", "")
                    .selected_text(RichText::new(format!("{cur}")).size(11.0))
                    .width(44.0)
                    .show_ui(ui, |ui| {
                        for sz in FONT_SIZES {
                            if ui
                                .selectable_label((sz - cur).abs() < 0.01, format!("{sz}"))
                                .clicked()
                            {
                                self.exec("home.font.size", json!({ "size": sz }));
                            }
                        }
                    })
                    .response
                    .on_hover_text(tip);
            }
            id @ ("home.font.fill_color" | "home.font.font_color") if on => {
                let fill = id == "home.font.fill_color";
                let id = id.to_string();
                ui.menu_button(RichText::new(&item.label).size(11.0).color(color), |ui| {
                    let none = if fill { "No Fill" } else { "Automatic" };
                    if ui.button(none).clicked() {
                        self.exec(&id, json!({ "color": null }));
                        ui.close();
                    }
                    if let Some(c) = swatch_grid(ui, None) {
                        self.exec(&id, json!({ "color": hex(c) }));
                        ui.close();
                    }
                    if ui.button("More Colors…").clicked() {
                        self.activate_command(&id);
                        ui.close();
                    }
                })
                .response
                .on_hover_text(tip);
            }
            _ => {
                let b = egui::Button::new(RichText::new(&item.label).size(11.0).color(color))
                    .small()
                    .frame(false);
                if ui.add_enabled(on, b).on_hover_text(tip).clicked() {
                    self.activate_command(&item.id);
                }
            }
        }
    }

    fn menu_cmd(&mut self, ui: &mut Ui, id: &str, text: &str) {
        if ui
            .add_enabled(enabled(&self.session, id), egui::Button::new(text))
            .clicked()
        {
            self.activate_command(id);
            ui.close();
        }
    }

    fn menu_exec(&mut self, ui: &mut Ui, id: &str, text: &str, params: serde_json::Value) {
        if ui
            .add_enabled(enabled(&self.session, id), egui::Button::new(text))
            .clicked()
        {
            self.exec(id, params);
            ui.close();
        }
    }

    /// Right-click menu on the cell grid.
    pub(crate) fn grid_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(170.0);
        self.menu_cmd(ui, "home.clipboard.cut", "Cut");
        self.menu_cmd(ui, "home.clipboard.copy", "Copy");
        self.menu_cmd(ui, "home.clipboard.paste", "Paste");
        self.menu_cmd(ui, "home.clipboard.paste_special", "Paste Special…");
        ui.separator();
        self.menu_cmd(ui, "home.cells.insert_rows", "Insert Rows");
        self.menu_cmd(ui, "home.cells.insert_columns", "Insert Columns");
        self.menu_cmd(ui, "home.cells.delete_rows", "Delete Rows");
        self.menu_cmd(ui, "home.cells.delete_columns", "Delete Columns");
        self.menu_cmd(ui, "home.editing.clear_contents", "Clear Contents");
        ui.separator();
        ui.menu_button("Sort", |ui| {
            self.menu_cmd(ui, "home.editing.sort_az", "Sort A to Z");
            self.menu_cmd(ui, "home.editing.sort_za", "Sort Z to A");
            self.menu_cmd(ui, "home.editing.sort_custom", "Custom Sort…");
        });
        let s = &self.session;
        let in_table =
            s.wb.sheet(s.sheet_index())
                .and_then(|sh| sh.table_at(s.sel.active))
                .is_some();
        if in_table && ui.button("Table Design…").clicked() {
            self.activate_command("table.design");
            ui.close();
        }
        self.menu_cmd(ui, "formulas.insert_function", "Insert Function…");
        self.menu_cmd(ui, "formulas.define_name", "Define Name…");
        ui.separator();
        self.menu_cmd(ui, "home.number.dialog", "Format Cells…");
        self.menu_cmd(ui, "home.cells.column_width", "Column Width…");
        self.menu_cmd(ui, "home.cells.row_height", "Row Height…");
    }

    /// Right-click menu on a sheet tab.
    pub(crate) fn tab_menu(&mut self, ui: &mut Ui, index: usize, name: &str) {
        ui.set_min_width(160.0);
        if ui.button("Insert…").clicked() {
            self.ui.dialog = Some(Dialog::Prompt(Prompt::insert_sheet()));
            ui.close();
        }
        let p = json!({ "sheet": name });
        self.menu_exec(ui, "home.cells.delete_sheet", "Delete", p.clone());
        if ui.button("Rename").clicked() {
            self.ui.dialog = Some(Dialog::Prompt(Prompt::rename_sheet(
                &self.session,
                Some(index),
            )));
            ui.close();
        }
        if ui.button("Move or Copy…").clicked() {
            if let Ok(mut d) = misc::Params::new("home.cells.move_copy_sheet") {
                d.text = json!({ "sheet": name, "to": index + 1, "copy": false }).to_string();
                self.ui.dialog = Some(Dialog::Params(d));
            }
            ui.close();
        }
        self.menu_exec(ui, "sheet.duplicate", "Duplicate", p.clone());
        ui.menu_button("Tab Color", |ui| {
            if ui.button("No Color").clicked() {
                self.exec(
                    "home.cells.tab_color",
                    json!({ "sheet": name, "color": null }),
                );
                ui.close();
            }
            if let Some(c) = swatch_grid(ui, None) {
                self.exec(
                    "home.cells.tab_color",
                    json!({ "sheet": name, "color": hex(c) }),
                );
                ui.close();
            }
            if ui.button("More…").clicked() {
                self.ui.dialog = Some(Dialog::TabColor(TabColor::new(&self.session, Some(index))));
                ui.close();
            }
        });
        ui.separator();
        self.menu_exec(ui, "home.cells.hide_sheet", "Hide", p);
        let any_hidden = self
            .session
            .wb
            .sheets
            .iter()
            .any(|s| s.visibility != sp_model::Visibility::Visible);
        if ui
            .add_enabled(any_hidden, egui::Button::new("Unhide…"))
            .clicked()
        {
            self.activate_command("home.cells.unhide_sheet");
            ui.close();
        }
    }
}
