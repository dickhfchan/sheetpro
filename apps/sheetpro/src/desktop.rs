//! The desktop window: native file dialogs, unsaved-changes prompts, menu bar, keyboard
//! shortcuts, autosave + recovery, extra windows, and the control channel. Every workbook
//! change still goes through `Session::execute` (via `sp_ui::App`).

use crate::ControlRx;
use crate::forms::Forms;
use egui::{Key, Modifiers};
use serde_json::json;
use sp_engine::Session;
use sp_engine::autosave::{self, Autosaver, Tick};
use sp_engine::commands::app_cmds;
use sp_engine::control::{Action, handle_line};
use sp_engine::settings::{self, Settings};
use sp_ui::App;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Command ids the desktop handles natively (native file dialogs, quitting).
pub use sp_ui::shell_hook::DESKTOP_INTERCEPTED as INTERCEPTED;

const WORKBOOK_EXTS: [&str; 5] = ["xlsx", "xlsm", "csv", "tsv", "txt"];
const TEXT_EXTS: [&str; 4] = ["csv", "tsv", "txt", "tab"];

pub struct Desktop {
    pub app: App,
    control: Option<ControlRx>,
    pub app_dir: Option<PathBuf>,
    pub prefs: Settings,
    autosaver: Option<Autosaver>,
    snapshot_job: Option<std::thread::JoinHandle<Result<PathBuf, String>>>,
    /// Close without prompting (control-channel quit, or the prompt already ran).
    force_close: bool,
    /// Last file path seen, to record recent files after open/save.
    last_path: Option<PathBuf>,
    last_status: String,
    /// `view.window.current` as left at the end of the previous frame.
    last_current: usize,
    pub forms: Forms,
}

impl Desktop {
    pub fn new(
        session: Session,
        control: Option<ControlRx>,
        app_dir: Option<PathBuf>,
        prefs: Settings,
    ) -> Self {
        sp_ui::shell_hook::set_intercepted(&INTERCEPTED);
        let autosaver = app_dir.as_ref().map(|d| {
            Autosaver::new(
                autosave::autosave_dir(d),
                format!("{}-{}", std::process::id(), settings::unix_now()),
                Duration::from_secs(prefs.autosave_interval_secs),
            )
        });
        let recovery = app_dir
            .as_ref()
            .map(|d| autosave::recoverable(&autosave::autosave_dir(d)))
            .unwrap_or_default();
        let last_path = session.path.clone();
        let forms = Forms {
            recovery,
            ..Default::default()
        };
        Desktop {
            app: App::new(session),
            control,
            app_dir,
            prefs,
            autosaver,
            snapshot_job: None,
            force_close: false,
            last_path,
            last_status: String::new(),
            last_current: 0,
            forms,
        }
    }

    /// Run a command; on failure show the error in a native alert. Returns success.
    pub fn run(&mut self, id: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        match self.app.try_exec(id, params) {
            Ok(v) => Some(v),
            Err(e) => {
                settings::log_line(&format!("{id} failed: {e}"));
                alert("SheetPro", &e);
                None
            }
        }
    }

    fn doc_name(&self) -> String {
        self.app
            .session
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("Book1")
            .to_string()
    }

    fn start_dir(&self) -> Option<PathBuf> {
        self.app
            .session
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .or_else(|| {
                self.prefs
                    .recent_files
                    .first()
                    .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
            })
    }

    // -----------------------------------------------------------------------------------
    // File actions

    /// Ask to save a dirty workbook. `true` = go ahead (saved or discarded).
    pub fn confirm_discard(&mut self) -> bool {
        if !self.app.session.dirty {
            return true;
        }
        let r = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("SheetPro")
            .set_description(format!(
                "Do you want to save the changes you made to {}?\n\nYour changes will be lost if you don't save them.",
                self.doc_name()
            ))
            .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
                "Save".into(),
                "Don't Save".into(),
                "Cancel".into(),
            ))
            .show();
        match r {
            rfd::MessageDialogResult::Yes => self.save(),
            rfd::MessageDialogResult::Custom(s) if s == "Save" => self.save(),
            rfd::MessageDialogResult::No => self.discard_autosave(),
            rfd::MessageDialogResult::Custom(s) if s == "Don't Save" => self.discard_autosave(),
            _ => false,
        }
    }

    fn discard_autosave(&mut self) -> bool {
        self.join_snapshot();
        if let Some(a) = &mut self.autosaver {
            a.finish();
        }
        true
    }

    /// Save to the current file, or ask for one. `true` when the workbook is now saved.
    pub fn save(&mut self) -> bool {
        let native = self
            .app
            .session
            .path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| WORKBOOK_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)));
        if native {
            self.run("file.save", json!({})).is_some() && !self.app.session.dirty
        } else {
            self.save_as("file.save_as")
        }
    }

    /// Save As / Save a Copy / Export CSV through a native save dialog.
    pub fn save_as(&mut self, id: &str) -> bool {
        let csv = id == "file.export.csv";
        let stem = Path::new(&self.doc_name())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Book1")
            .to_string();
        let mut d = rfd::FileDialog::new()
            .set_title(match id {
                "file.save_copy" => "Save a Copy",
                "file.export.csv" => "Export as CSV",
                _ => "Save As",
            })
            .set_file_name(format!("{stem}.{}", if csv { "csv" } else { "xlsx" }));
        d = if csv {
            d.add_filter("CSV (comma delimited)", &["csv"])
        } else {
            d.add_filter("Excel Workbook", &["xlsx"])
                .add_filter("CSV (comma delimited)", &["csv"])
                .add_filter("Text (tab delimited)", &["tsv", "txt"])
        };
        if let Some(dir) = self.start_dir() {
            d = d.set_directory(dir);
        }
        let Some(mut path) = d.save_file() else {
            return false;
        };
        if path.extension().is_none() {
            path.set_extension(if csv { "csv" } else { "xlsx" });
        }
        let ok = self
            .run(id, json!({ "path": path.display().to_string() }))
            .is_some();
        if ok {
            app_cmds::note_recent(&path);
            settings::log_line(&format!("{id} {}", path.display()));
        }
        ok && (id != "file.save_as" || !self.app.session.dirty)
    }

    pub fn open_dialog(&mut self) {
        if !self.confirm_discard() {
            return;
        }
        let mut d = rfd::FileDialog::new()
            .set_title("Open")
            .add_filter("Spreadsheets", &WORKBOOK_EXTS)
            .add_filter("All files", &["*"]);
        if let Some(dir) = self.start_dir() {
            d = d.set_directory(dir);
        }
        if let Some(path) = d.pick_file() {
            self.open_path(&path);
        }
    }

    /// Open a file (the unsaved prompt has already run).
    pub fn open_path(&mut self, path: &Path) {
        if self
            .run("file.open", json!({ "path": path.display().to_string() }))
            .is_some()
        {
            app_cmds::note_recent(path);
            settings::log_line(&format!("opened {}", path.display()));
        }
    }

    pub fn new_workbook(&mut self) {
        if self.confirm_discard() && self.run("file.new", json!({})).is_some() {
            self.prefs.apply_to_new(&mut self.app.session);
        }
    }

    pub fn close_workbook(&mut self) {
        if self.confirm_discard() && self.run("file.close", json!({"discard": true})).is_some() {
            self.prefs.apply_to_new(&mut self.app.session);
        }
    }

    /// Export as PDF through a native save dialog (the active sheet).
    pub fn export_pdf(&mut self) {
        let stem = Path::new(&self.doc_name())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Book1")
            .to_string();
        let mut d = rfd::FileDialog::new()
            .set_title("Export as PDF")
            .set_file_name(format!("{stem}.pdf"))
            .add_filter("PDF", &["pdf"]);
        if let Some(dir) = self.start_dir() {
            d = d.set_directory(dir);
        }
        if let Some(mut path) = d.save_file() {
            if path.extension().is_none() {
                path.set_extension("pdf");
            }
            if self
                .run(
                    "file.export.pdf",
                    json!({ "path": path.display().to_string() }),
                )
                .is_some()
            {
                settings::log_line(&format!("exported {}", path.display()));
            }
        }
    }

    /// Insert Picture / sheet Background: pick an image through a native open dialog.
    pub fn pick_image(&mut self, id: &str) {
        let mut d = rfd::FileDialog::new()
            .set_title(if id == "layout.background" {
                "Sheet Background"
            } else {
                "Insert Picture"
            })
            .add_filter("Pictures", &["png", "jpg", "jpeg", "gif", "bmp"]);
        if let Some(dir) = self.start_dir() {
            d = d.set_directory(dir);
        }
        if let Some(path) = d.pick_file() {
            self.run(id, json!({ "path": path.display().to_string() }));
        }
    }

    pub fn import_text(&mut self) {
        let mut d = rfd::FileDialog::new()
            .set_title("Import Text/CSV")
            .add_filter("Text files", &TEXT_EXTS)
            .add_filter("All files", &["*"]);
        if let Some(dir) = self.start_dir() {
            d = d.set_directory(dir);
        }
        if let Some(path) = d.pick_file()
            && let Some(r) = self.run(
                "data.get_from_text",
                json!({ "path": path.display().to_string() }),
            )
        {
            settings::log_line(&format!(
                "imported {} → {}",
                path.display(),
                r.get("sheet").unwrap_or(&json!(null))
            ));
        }
    }

    pub fn request_quit(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
    }

    /// Dispatch a desktop action by command id (menu, shortcut or intercepted ribbon click).
    pub fn action(&mut self, ctx: &egui::Context, id: &str) {
        match id {
            "file.new" => self.new_workbook(),
            "file.open" => self.open_dialog(),
            "file.save" => {
                self.save();
            }
            "file.save_as" | "file.save_copy" | "file.export.csv" => {
                self.save_as(id);
            }
            "file.export.pdf" => self.export_pdf(),
            "insert.picture" | "layout.background" => self.pick_image(id),
            "file.close" => self.close_workbook(),
            "file.exit" => self.request_quit(ctx),
            "file.recent" => {
                self.prefs = settings::current_or(&self.prefs);
                self.forms.recent_open = true;
            }
            "file.options" => self.forms.open_options(&self.prefs),
            "file.inspect" => {
                let r = self.app.query("file.inspect", json!({})).ok();
                self.forms.open_inspector(r);
            }
            "data.get_from_text" => self.import_text(),
            "app.report_problem" => self.forms.report_open = true,
            other => self.app.activate_command(other),
        }
    }

    /// Desktop shortcuts, consumed before the shell sees them (Cmd on macOS, Ctrl elsewhere).
    fn shortcuts(&mut self, ctx: &egui::Context) -> Vec<&'static str> {
        let mut out = Vec::new();
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        ctx.input_mut(|i| {
            if i.consume_key(cmd_shift, Key::S) {
                out.push("file.save_as");
            }
            for (key, id) in [
                (Key::N, "file.new"),
                (Key::O, "file.open"),
                (Key::S, "file.save"),
                (Key::W, "file.close"),
                (Key::Q, "file.exit"),
            ] {
                if i.consume_key(cmd, key) {
                    out.push(id);
                }
            }
            if i.consume_key(Modifiers::NONE, Key::F12) {
                out.push("file.save_as");
            }
        });
        out
    }

    // -----------------------------------------------------------------------------------
    // Autosave

    fn join_snapshot(&mut self) {
        if let Some(job) = self.snapshot_job.take() {
            match job.join() {
                Ok(Err(e)) => settings::log_line(&format!("autosave failed: {e}")),
                Err(_) => settings::log_line("autosave thread panicked"),
                Ok(Ok(_)) => {}
            }
        }
    }

    fn autosave_tick(&mut self) {
        if self.snapshot_job.as_ref().is_some_and(|j| j.is_finished()) {
            self.join_snapshot();
        }
        let Some(a) = &mut self.autosaver else {
            return;
        };
        a.interval = Duration::from_secs(self.prefs.autosave_interval_secs);
        if self.snapshot_job.is_some() {
            return; // previous snapshot still writing
        }
        match a.tick(&self.app.session, Instant::now()) {
            Tick::Snapshot { id } => {
                let s = &self.app.session;
                let (wb, path, title, dir) =
                    (s.wb.clone(), s.path.clone(), s.title(), a.dir.clone());
                self.snapshot_job = Some(std::thread::spawn(move || {
                    autosave::write_snapshot(&dir, &id, &wb, path.as_deref(), &title)
                }));
            }
            Tick::Discarded { .. } | Tick::Idle => {}
        }
    }

    /// Replace the session with a recovered snapshot.
    pub fn recover(&mut self, idx: usize) {
        let Some(snap) = self.forms.recovery.get(idx).cloned() else {
            return;
        };
        if !self.confirm_discard() {
            return;
        }
        match autosave::recover(&snap) {
            Ok(s) => {
                self.app = App::new(s);
                if let Some(a) = &mut self.autosaver {
                    if let Err(e) = a.snapshot_now(&self.app.session) {
                        settings::log_line(&format!("autosave failed: {e}"));
                    }
                    autosave::discard(&a.dir, &snap.meta.id);
                }
                self.forms.recovery.retain(|x| x.meta.id != snap.meta.id);
                settings::log_line(&format!("recovered snapshot {}", snap.meta.id));
            }
            Err(e) => alert("SheetPro", &format!("Could not recover: {e}")),
        }
    }

    pub fn discard_recovery(&mut self, idx: usize) {
        if let (Some(snap), Some(a)) = (self.forms.recovery.get(idx).cloned(), &self.autosaver) {
            autosave::discard(&a.dir, &snap.meta.id);
            self.forms.recovery.retain(|x| x.meta.id != snap.meta.id);
        }
    }

    // -----------------------------------------------------------------------------------
    // Frame

    fn control(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.control else {
            return;
        };
        let mut quit = false;
        let mut any = false;
        while let Ok((line, reply)) = rx.try_recv() {
            any = true;
            let (out, action) = handle_line(&mut self.app.session, &line);
            match action {
                Action::Quit => quit = true,
                // Captured after the next frame; `save_screenshots` writes the file.
                Action::Screenshot(path) => ctx.send_viewport_cmd_to(
                    egui::ViewportId::ROOT,
                    egui::ViewportCommand::Screenshot(egui::UserData::new(PathBuf::from(path))),
                ),
                _ => {}
            }
            let _ = reply.send(out);
        }
        if any {
            // file.options / file.recent may have changed the settings file.
            self.prefs = settings::current_or(&self.prefs);
        }
        if quit {
            self.force_close = true;
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    /// Menu bar + shortcuts + the shell for one window.
    fn window_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut actions: Vec<String> = self
            .shortcuts(&ctx)
            .into_iter()
            .map(str::to_string)
            .collect();
        egui::Panel::top("menubar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                crate::forms::menu_bar(self, ui, &mut actions);
            });
        });
        self.app.show(ui);
        actions.extend(sp_ui::shell_hook::take_pending());
        for id in actions {
            self.action(&ctx, &id);
        }
    }

    fn bookkeeping(&mut self) {
        // Recent files: a new path that is saved on disk (opened or saved elsewhere, e.g.
        // through the control channel).
        let s = &self.app.session;
        if s.path != self.last_path {
            if let Some(p) = &s.path
                && !s.dirty
            {
                app_cmds::note_recent(p);
                self.prefs = settings::current();
            }
            self.last_path = s.path.clone();
        }
        if s.status != self.last_status {
            self.last_status = s.status.clone();
            if s.status.starts_with("Saved") || s.status.starts_with("Opened") {
                settings::log_line(&s.status);
            }
        }
    }
}

/// Write the window captures requested by the control `screenshot` op (PNG).
fn save_screenshots(ctx: &egui::Context) {
    let shots: Vec<(PathBuf, std::sync::Arc<egui::ColorImage>)> = ctx.input(|i| {
        i.raw
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Screenshot {
                    user_data, image, ..
                } => user_data
                    .data
                    .as_ref()
                    .and_then(|d| d.downcast_ref::<PathBuf>())
                    .map(|p| (p.clone(), image.clone())),
                _ => None,
            })
            .collect()
    });
    for (path, img) in shots {
        let [w, h] = img.size;
        let px: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        let saved = u32::try_from(w)
            .ok()
            .zip(u32::try_from(h).ok())
            .and_then(|(w, h)| image::RgbaImage::from_raw(w, h, px))
            .map(|im| im.save(&path));
        if !matches!(saved, Some(Ok(()))) {
            settings::log_line(&format!("screenshot: could not write {}", path.display()));
        }
    }
}

impl eframe::App for Desktop {
    /// Runs before every UI pass and also while the window is hidden or minimized (when the
    /// control server requests a repaint), so control requests never wait for a visible window.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.control(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        save_screenshots(&ctx);
        // Drawing preferences (gridline colour, R1C1 headings) follow the settings.
        sp_ui::shell_hook::apply_settings(&self.prefs);

        // Which session window each viewport shows: the root shows the first visible one.
        let wins = self.app.session.view.window.windows.clone();
        let multi = wins.len() > 1;
        let root_idx = wins.iter().position(|w| !w.hidden).unwrap_or(0);
        let commanded = self.app.session.view.window.current != self.last_current;
        let mut focused_idx: Option<usize> = None;
        if multi {
            app_cmds::activate_window(&mut self.app.session, root_idx);
        }
        self.window_ui(ui);
        if multi && ctx.input(|i| i.viewport().focused).unwrap_or(false) {
            focused_idx = Some(self.app.session.view.window.current);
        }

        // Secondary windows (View › New Window) as native viewports on the same session.
        let mut closed: Vec<usize> = Vec::new();
        if multi {
            let base = self.app.session.title();
            for (i, w) in wins.iter().enumerate() {
                if i == root_idx || w.hidden {
                    continue;
                }
                let vid = egui::ViewportId::from_hash_of(("sheetpro-window", w.id));
                let builder = egui::ViewportBuilder::default()
                    .with_title(format!("{base} : {}", w.id))
                    .with_inner_size([960.0, 640.0]);
                ctx.show_viewport_immediate(vid, builder, |ui, _class| {
                    app_cmds::activate_window(&mut self.app.session, i);
                    self.window_ui(ui);
                    let vctx = ui.ctx().clone();
                    if vctx.input(|inp| inp.viewport().close_requested()) {
                        closed.push(i);
                    }
                    if vctx.input(|inp| inp.viewport().focused).unwrap_or(false) {
                        focused_idx = Some(self.app.session.view.window.current);
                    }
                });
            }
            // A command (Switch Windows) picked a window: focus it; otherwise the focused
            // window is the current one for the control channel and the next frame.
            let target = self.app.session.view.window.current;
            if commanded {
                let id = wins.get(target).map_or(0, |w| w.id);
                let vid = if target == root_idx {
                    egui::ViewportId::ROOT
                } else {
                    egui::ViewportId::from_hash_of(("sheetpro-window", id))
                };
                ctx.send_viewport_cmd_to(vid, egui::ViewportCommand::Focus);
            } else if let Some(f) = focused_idx {
                app_cmds::activate_window(&mut self.app.session, f);
            }
            for i in closed.into_iter().rev() {
                app_cmds::close_window(&mut self.app.session, i);
            }
        }
        self.last_current = self.app.session.view.window.current;

        if self.app.session.view.window.minimize_requested {
            self.app.session.view.window.minimize_requested = false;
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                egui::ViewportCommand::Minimized(true),
            );
        }

        crate::forms::show(self, &ctx);
        self.autosave_tick();
        self.bookkeeping();

        // file.exit via the control channel or the shell already checked for changes.
        if self.app.session.quit_requested {
            self.app.session.quit_requested = false;
            self.force_close = true;
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            let forced = self.force_close;
            if forced || self.confirm_discard() {
                self.force_close = true;
                self.join_snapshot();
                if let Some(a) = &mut self.autosaver {
                    if forced && self.app.session.dirty {
                        // Forced quit with unsaved changes: keep them recoverable.
                        if let Err(e) = a.snapshot_now(&self.app.session) {
                            settings::log_line(&format!("autosave failed: {e}"));
                        }
                    } else if !self.app.session.dirty {
                        a.finish();
                    }
                }
                settings::log_line("window closed");
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }
        let title = self.app.session.title();
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Title(title));
        // Autosave needs a heartbeat even when idle.
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

/// Native error alert.
pub fn alert(title: &str, msg: &str) {
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(msg)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(test)]
mod tests {
    #[test]
    fn intercepted_ids_are_registered_commands() {
        for id in super::INTERCEPTED {
            assert!(
                sp_engine::registry().get(id).is_some(),
                "{id} is not a registered command"
            );
        }
    }
}
