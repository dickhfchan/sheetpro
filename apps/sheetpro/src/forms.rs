//! Desktop-only windows (menu bar, recovery, recent files, options, document inspector,
//! problem report). They collect input and call commands; nothing here edits the workbook.

use crate::desktop::Desktop;
use serde_json::{Value as Json, json};
use sp_engine::autosave::Snapshot;
use sp_engine::settings::{self, Settings};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Forms {
    /// Snapshots offered for recovery (from sessions that ended without saving).
    pub recovery: Vec<Snapshot>,
    pub recent_open: bool,
    pub options: Option<Settings>,
    pub inspector: Option<Inspector>,
    pub report_open: bool,
    pub report_note: String,
    pub report_include_workbook: bool,
    pub report_result: Option<String>,
    pub about_open: bool,
}

pub struct Inspector {
    pub report: Json,
    pub remove: [bool; 4],
}

const REMOVABLE: [(&str, &str); 4] = [
    ("hidden_sheets", "Hidden sheets"),
    ("comments", "Comments and notes"),
    ("personal_info", "Document author and comment authors"),
    (
        "external_links",
        "External links (formulas keep their values)",
    ),
];

impl Forms {
    pub fn open_options(&mut self, current: &Settings) {
        self.options = Some(settings::current_or(current));
    }

    pub fn open_inspector(&mut self, report: Option<Json>) {
        self.inspector = report.map(|report| Inspector {
            report,
            remove: [false; 4],
        });
    }
}

/// The menu bar above the ribbon. Actions are command ids run after the frame's UI.
pub fn menu_bar(d: &mut Desktop, ui: &mut egui::Ui, actions: &mut Vec<String>) {
    let mut act = |id: &str| actions.push(id.to_string());
    let cmd = |k: &str| {
        if cfg!(target_os = "macos") {
            format!("⌘{k}")
        } else {
            format!("Ctrl+{k}")
        }
    };
    ui.menu_button("File", |ui| {
        if ui
            .add(egui::Button::new("New").shortcut_text(cmd("N")))
            .clicked()
        {
            act("file.new");
        }
        if ui
            .add(egui::Button::new("Open…").shortcut_text(cmd("O")))
            .clicked()
        {
            act("file.open");
        }
        ui.menu_button("Open Recent", |ui| {
            if d.prefs.recent_files.is_empty() {
                ui.label("No recent files");
            }
            for f in d.prefs.recent_files.clone() {
                let name = Path::new(&f)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&f)
                    .to_string();
                if ui.button(name).on_hover_text(&f).clicked() {
                    open_recent(d, &f);
                }
            }
            ui.separator();
            if ui.button("Clear Recent").clicked() {
                let _ = d.app.try_exec("file.recent", json!({"op": "clear"}));
                d.prefs = settings::current();
            }
        });
        ui.separator();
        if ui
            .add(egui::Button::new("Save").shortcut_text(cmd("S")))
            .clicked()
        {
            act("file.save");
        }
        if ui
            .add(egui::Button::new("Save As…").shortcut_text(cmd("⇧S")))
            .clicked()
        {
            act("file.save_as");
        }
        if ui.button("Save a Copy…").clicked() {
            act("file.save_copy");
        }
        ui.separator();
        if ui.button("Import Text/CSV…").clicked() {
            act("data.get_from_text");
        }
        if ui.button("Export as CSV…").clicked() {
            act("file.export.csv");
        }
        ui.separator();
        if ui.button("Inspect Document…").clicked() {
            act("file.inspect");
        }
        if ui.button("Options…").clicked() {
            act("file.options");
        }
        ui.separator();
        if ui
            .add(egui::Button::new("Close").shortcut_text(cmd("W")))
            .clicked()
        {
            act("file.close");
        }
        if ui
            .add(egui::Button::new("Quit").shortcut_text(cmd("Q")))
            .clicked()
        {
            act("file.exit");
        }
    });
    ui.menu_button("Window", |ui| {
        if ui.button("New Window").clicked() {
            act("view.new_window");
        }
        let split = d.app.session.view.window.split.is_some();
        if ui
            .button(if split { "Remove Split" } else { "Split" })
            .clicked()
        {
            act("view.split");
        }
        if ui.button("Switch Windows").clicked() {
            act("view.switch_windows");
        }
        if ui.button("Hide Window").clicked() {
            act("view.hide_window");
        }
        if ui.button("Unhide Windows").clicked() {
            let _ = d.app.try_exec("view.hide_window", json!({"unhide": true}));
        }
    });
    ui.menu_button("Help", |ui| {
        if ui.button("Report a Problem…").clicked() {
            act("app.report_problem");
        }
        if ui.button("Show Reports Folder").clicked()
            && let Some(dir) = &d.app_dir
        {
            reveal(&dir.join("reports"));
        }
        if ui.button("About SheetPro").clicked() {
            d.forms.about_open = true;
        }
    });
}

fn open_recent(d: &mut Desktop, f: &str) {
    if d.confirm_discard() {
        d.open_path(Path::new(f));
        d.prefs = settings::current();
    }
}

/// Show a folder in the platform file manager (local process; creates the folder first).
pub fn reveal(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let tool = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(tool).arg(dir).spawn();
}

/// `v[k1][k2]…` without indexing (missing keys give `null`).
fn jget<'a>(v: &'a Json, keys: &[&str]) -> &'a Json {
    keys.iter()
        .try_fold(v, |v, k| v.get(*k))
        .unwrap_or(&Json::Null)
}

fn time_text(unix: u64) -> String {
    let age = settings::unix_now().saturating_sub(unix);
    match age {
        0..=89 => "just now".into(),
        90..=5399 => format!("{} minutes ago", age / 60),
        5400..=129_599 => format!("{} hours ago", age / 3600),
        _ => format!("{} days ago", age / 86_400),
    }
}

/// All desktop windows for this frame.
pub fn show(d: &mut Desktop, ctx: &egui::Context) {
    recovery(d, ctx);
    recent(d, ctx);
    options(d, ctx);
    inspector(d, ctx);
    report(d, ctx);
    about(d, ctx);
}

fn recovery(d: &mut Desktop, ctx: &egui::Context) {
    if d.forms.recovery.is_empty() {
        return;
    }
    let mut recover: Option<usize> = None;
    let mut discard: Option<usize> = None;
    let mut discard_all = false;
    egui::Window::new("Document Recovery")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label("SheetPro closed before these workbooks were saved. Recover them?");
            ui.add_space(6.0);
            egui::Grid::new("recovery").striped(true).show(ui, |ui| {
                for (i, s) in d.forms.recovery.iter().enumerate() {
                    let name = s
                        .meta
                        .original
                        .clone()
                        .unwrap_or_else(|| s.meta.title.clone());
                    ui.label(name);
                    ui.label(time_text(s.meta.saved_at));
                    if ui.button("Recover").clicked() {
                        recover = Some(i);
                    }
                    if ui.button("Discard").clicked() {
                        discard = Some(i);
                    }
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Discard All").clicked() {
                    discard_all = true;
                }
                if ui.button("Decide Later").clicked() {
                    // Keep the files; ask again next launch.
                    d.forms.recovery.clear();
                }
            });
        });
    if let Some(i) = recover {
        d.recover(i);
    } else if let Some(i) = discard {
        d.discard_recovery(i);
    } else if discard_all {
        while !d.forms.recovery.is_empty() {
            d.discard_recovery(0);
        }
    }
}

fn recent(d: &mut Desktop, ctx: &egui::Context) {
    if !d.forms.recent_open {
        return;
    }
    let mut open = true;
    let mut pick: Option<String> = None;
    egui::Window::new("Open Recent")
        .open(&mut open)
        .collapsible(false)
        .show(ctx, |ui| {
            if d.prefs.recent_files.is_empty() {
                ui.label("No recent files yet.");
            }
            for f in &d.prefs.recent_files {
                let exists = Path::new(f).exists();
                let b = ui.add_enabled(exists, egui::Button::new(f.as_str()));
                if b.clicked() {
                    pick = Some(f.clone());
                }
            }
        });
    if let Some(f) = pick {
        open = false;
        open_recent(d, &f);
    }
    d.forms.recent_open = open;
}

fn options(d: &mut Desktop, ctx: &egui::Context) {
    let Some(mut form) = d.forms.options.take() else {
        return;
    };
    let mut keep = true;
    let mut apply = false;
    egui::Window::new("Options")
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("options").num_columns(2).show(ui, |ui| {
                ui.label("Autosave every (seconds, 0 = off)");
                ui.add(egui::DragValue::new(&mut form.autosave_interval_secs).range(0..=86_400));
                ui.end_row();
                ui.label("Default font");
                ui.text_edit_singleline(&mut form.default_font);
                ui.end_row();
                ui.label("Default font size");
                ui.add(
                    egui::DragValue::new(&mut form.default_font_size)
                        .range(1.0..=409.0)
                        .speed(0.5),
                );
                ui.end_row();
                ui.label("Calculation");
                egui::ComboBox::from_id_salt("calc_mode")
                    .selected_text(form.calc_mode.clone())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut form.calc_mode, "automatic".into(), "automatic");
                        ui.selectable_value(&mut form.calc_mode, "manual".into(), "manual");
                    });
                ui.end_row();
                ui.label("R1C1 reference style");
                ui.checkbox(&mut form.r1c1, "(headings and Name Box; formulas stay A1)");
                ui.end_row();
                ui.label("Gridline colour");
                let mut rgb = form.gridline_rgb().unwrap_or([0xDA, 0xDC, 0xE0]);
                ui.horizontal(|ui| {
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        form.gridline_color =
                            format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut form.gridline_color).desired_width(70.0),
                    );
                    if ui.small_button("Default").clicked() {
                        form.gridline_color = settings::DEFAULT_GRIDLINE.to_string();
                    }
                });
                ui.end_row();
                ui.label("Recent files to show");
                ui.add(
                    egui::DragValue::new(&mut form.recent_count).range(0..=settings::MAX_RECENT),
                );
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
        });
    if apply {
        let set = json!({
            "autosave_interval_secs": form.autosave_interval_secs,
            "default_font": form.default_font,
            "default_font_size": form.default_font_size,
            "calc_mode": form.calc_mode,
            "r1c1": form.r1c1,
            "gridline_color": form.gridline_color,
            "recent_count": form.recent_count,
        });
        match d.app.try_exec("file.options", json!({ "set": set })) {
            Ok(_) => {
                d.prefs = settings::current();
                keep = false;
            }
            Err(e) => crate::desktop::alert("Options", &e),
        }
    }
    if keep {
        d.forms.options = Some(form);
    }
}

fn inspector(d: &mut Desktop, ctx: &egui::Context) {
    let Some(mut form) = d.forms.inspector.take() else {
        return;
    };
    let mut keep = true;
    let mut remove = false;
    let r = &form.report;
    let count = |v: &Json| v.as_array().map_or(0, Vec::len);
    let lines = [
        format!("Hidden sheets: {}", count(jget(r, &["hidden_sheets"]))),
        format!(
            "Comments and notes: {} (authors: {})",
            jget(r, &["comments", "count"]),
            count(jget(r, &["comments", "authors"]))
        ),
        format!(
            "Document author: {}",
            jget(r, &["personal_info", "author"])
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("(none)")
        ),
        format!(
            "External links: {} ({} formulas)",
            count(jget(r, &["external_links"])),
            jget(r, &["external_formulas"])
        ),
    ];
    let extra = format!(
        "Hidden rows: {}   hidden columns: {}   macros: {}   custom XML parts: {}",
        jget(r, &["hidden_rows"]),
        jget(r, &["hidden_columns"]),
        jget(r, &["macros"]),
        jget(r, &["custom_xml_parts"])
    );
    egui::Window::new("Inspect Document")
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            for (i, line) in lines.iter().enumerate() {
                ui.horizontal(|ui| {
                    if let (Some(flag), Some((_, label))) =
                        (form.remove.get_mut(i), REMOVABLE.get(i))
                    {
                        ui.checkbox(flag, "")
                            .on_hover_text(format!("Remove: {label}"));
                    }
                    ui.label(line);
                });
            }
            ui.label(egui::RichText::new(extra).small());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Remove Checked").clicked() {
                    remove = true;
                }
                if ui.button("Close").clicked() {
                    keep = false;
                }
            });
        });
    if remove {
        let keys: Vec<&str> = REMOVABLE
            .iter()
            .zip(form.remove.iter())
            .filter(|(_, on)| **on)
            .map(|((k, _), _)| *k)
            .collect();
        if !keys.is_empty() {
            match d.app.try_exec("file.inspect", json!({ "remove": keys })) {
                Ok(r) => {
                    form.report = r;
                    form.remove = [false; 4];
                }
                Err(e) => crate::desktop::alert("Inspect Document", &e),
            }
        }
    }
    if keep {
        d.forms.inspector = Some(form);
    }
}

fn report(d: &mut Desktop, ctx: &egui::Context) {
    if !d.forms.report_open {
        return;
    }
    let mut open = true;
    let mut send = false;
    let mut show_folder: Option<PathBuf> = None;
    egui::Window::new("Report a Problem")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Describe what happened. The report is saved on this computer only;");
            ui.label("nothing is sent anywhere. Attach the folder when you report the issue.");
            ui.add(
                egui::TextEdit::multiline(&mut d.forms.report_note)
                    .desired_rows(5)
                    .desired_width(420.0),
            );
            ui.checkbox(
                &mut d.forms.report_include_workbook,
                "Include a copy of the current workbook",
            );
            ui.horizontal(|ui| {
                if ui.button("Save Report").clicked() {
                    send = true;
                }
                if let Some(p) = &d.forms.report_result
                    && ui.button("Show in Folder").clicked()
                {
                    show_folder = Some(PathBuf::from(p));
                }
            });
            if let Some(p) = &d.forms.report_result {
                ui.label(format!("Saved to {p}"));
            }
        });
    if send {
        let params = json!({
            "note": d.forms.report_note,
            "include_workbook": d.forms.report_include_workbook,
        });
        match d.app.try_exec("app.report_problem", params) {
            Ok(p) => d.forms.report_result = p.as_str().map(str::to_string),
            Err(e) => crate::desktop::alert("Report a Problem", &e),
        }
    }
    if let Some(p) = show_folder {
        reveal(&p);
    }
    if !open {
        d.forms.report_result = None;
        d.forms.report_note.clear();
    }
    d.forms.report_open = open;
}

fn about(d: &mut Desktop, ctx: &egui::Context) {
    let mut open = d.forms.about_open;
    egui::Window::new("About SheetPro")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(format!("SheetPro {}", env!("CARGO_PKG_VERSION")));
            ui.label("An open-source spreadsheet. Not affiliated with Microsoft.");
            if let Some(dir) = &d.app_dir {
                ui.label(format!("Data folder: {}", dir.display()));
            }
        });
    d.forms.about_open = open;
}
