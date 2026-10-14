//! L5 engine: `Session` (open workbook + selection + history + view state) and the command
//! registry. Every user-visible action is a command dispatched by id.

pub mod autosave;
pub mod catalog;
pub mod commands;
pub mod control;
pub mod params;
pub mod settings;

use serde::Serialize;
use sp_core::{CellAddr, CellRange};
use sp_formula::{AstCache, RecalcOptions, RecalcStats};
use sp_model::{CalcMode, Workbook};
use sp_ops::{ClipData, History, Tx};
use std::path::PathBuf;
use std::sync::OnceLock;

pub use serde_json::Value as Json;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CmdError {
    #[error("unknown command '{0}'")]
    Unknown(String),
    #[error("command '{0}' is not available right now")]
    Disabled(String),
    #[error("invalid parameter: {0}")]
    Param(String),
    #[error("{0}")]
    Failed(String),
}

impl From<sp_ops::OpError> for CmdError {
    fn from(e: sp_ops::OpError) -> Self {
        CmdError::Failed(e.to_string())
    }
}

impl From<sp_io::IoError> for CmdError {
    fn from(e: sp_io::IoError) -> Self {
        CmdError::Failed(e.to_string())
    }
}

pub type CmdResult = Result<Json, CmdError>;
pub type RunFn = fn(&mut Session, &Json) -> CmdResult;
pub type EnabledFn = fn(&Session) -> bool;

/// What a command module registers. Label, location and shortcut come from the catalog.
#[derive(Clone, Copy)]
pub struct CommandSpec {
    pub id: &'static str,
    /// Agent-readable params doc, e.g. `{"range"?: "A1:B2"}`.
    pub params: &'static str,
    pub enabled: EnabledFn,
    pub run: RunFn,
}

pub fn always(_: &Session) -> bool {
    true
}

impl CommandSpec {
    pub const fn new(id: &'static str, params: &'static str, run: RunFn) -> Self {
        CommandSpec {
            id,
            params,
            enabled: always,
            run,
        }
    }

    pub const fn when(mut self, enabled: EnabledFn) -> Self {
        self.enabled = enabled;
        self
    }
}

/// A registered command with its catalog metadata.
#[derive(Clone, Serialize)]
pub struct Command {
    pub id: &'static str,
    pub label: String,
    pub location: String,
    pub shortcut: String,
    pub params: &'static str,
    #[serde(skip)]
    pub spec: CommandSpec,
}

pub struct Registry {
    commands: Vec<Command>,
}

impl Registry {
    pub fn get(&self, id: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.id == id)
    }

    pub fn all(&self) -> &[Command] {
        &self.commands
    }

    pub fn by_shortcut(&self, key: &str) -> Option<&Command> {
        self.commands
            .iter()
            .find(|c| !c.shortcut.is_empty() && c.shortcut.eq_ignore_ascii_case(key))
    }
}

pub fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| {
        let mut commands: Vec<Command> = Vec::new();
        for spec in commands::all_specs() {
            if commands.iter().any(|c| c.id == spec.id) {
                continue;
            }
            let item = catalog::find(spec.id);
            commands.push(Command {
                id: spec.id,
                label: item.map_or_else(|| spec.id.to_string(), |i| i.label.clone()),
                location: item.map_or_else(String::new, |i| i.loc.clone()),
                shortcut: item.map_or_else(String::new, |i| i.key.clone()),
                params: spec.params,
                spec,
            });
        }
        Registry { commands }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Selection {
    /// Cell receiving input.
    pub active: CellAddr,
    /// Fixed corner of the selection; `range` spans `anchor`..`cursor`.
    pub anchor: CellAddr,
    /// Moving corner (extended by Shift+arrows).
    pub cursor: CellAddr,
}

impl Selection {
    pub fn at(a: CellAddr) -> Self {
        Selection {
            active: a,
            anchor: a,
            cursor: a,
        }
    }

    pub fn range(&self) -> CellRange {
        CellRange::new(self.anchor, self.cursor)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ViewState {
    pub zoom: f32,
    pub show_formulas: bool,
    pub show_gridlines: bool,
    pub show_headings: bool,
    pub show_formula_bar: bool,
    pub full_screen: bool,
    /// Top-left visible cell of the scrollable pane.
    pub scroll: CellAddr,
    /// Rows/cols that fit on screen (set by the shell; used by page up/down).
    pub page_rows: u32,
    pub page_cols: u32,
    /// The shell is editing the active cell.
    pub editing: bool,
    pub ribbon_tab: String,
    /// Tracer arrows, watches, validation circles, comment pane (Formulas/Review tabs).
    pub review: commands::audit_cmds::ReviewState,
    /// Extra windows and split panes (View › Window).
    pub window: commands::app_cmds::WindowState,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            zoom: 1.0,
            show_formulas: false,
            show_gridlines: true,
            show_headings: true,
            show_formula_bar: true,
            full_screen: false,
            scroll: CellAddr::default(),
            page_rows: 30,
            page_cols: 10,
            editing: false,
            ribbon_tab: "Home".into(),
            review: commands::audit_cmds::ReviewState::default(),
            window: commands::app_cmds::WindowState::default(),
        }
    }
}

pub struct Session {
    pub wb: Workbook,
    pub sel: Selection,
    pub history: History,
    pub clipboard: Option<ClipData>,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    pub view: ViewState,
    pub status: String,
    pub cache: AstCache,
    pub recalc_opts: RecalcOptions,
    pub last_recalc: RecalcStats,
    pub last_find: Option<String>,
    /// Ids of the last command run (for edit.repeat).
    pub last_command: Option<(String, Json)>,
    pub quit_requested: bool,
    /// Set while a typing command runs: data validation applies to the cells it changes.
    pub validate_entry: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Undo depth (the target app keeps 100 levels).
pub const UNDO_LIMIT: usize = 100;

impl Session {
    pub fn new() -> Self {
        Session::with_workbook(Workbook::new())
    }

    pub fn with_workbook(wb: Workbook) -> Self {
        let mut s = Session {
            wb,
            sel: Selection::at(CellAddr::default()),
            history: History::new(UNDO_LIMIT),
            clipboard: None,
            path: None,
            dirty: false,
            view: ViewState::default(),
            status: "Ready".into(),
            cache: AstCache::default(),
            recalc_opts: RecalcOptions::default(),
            last_recalc: RecalcStats::default(),
            last_find: None,
            last_command: None,
            quit_requested: false,
            validate_entry: false,
        };
        s.recalc();
        s
    }

    pub fn sheet_index(&self) -> usize {
        self.wb.active.min(self.wb.sheets.len().saturating_sub(1))
    }

    pub fn recalc(&mut self) {
        self.last_recalc = sp_formula::recalc(&mut self.wb, &mut self.cache, self.recalc_opts);
    }

    /// Run a command by id.
    pub fn execute(&mut self, id: &str, params: &Json) -> CmdResult {
        let cmd = registry()
            .get(id)
            .ok_or_else(|| CmdError::Unknown(id.to_string()))?;
        if !(cmd.spec.enabled)(self) {
            return Err(CmdError::Disabled(id.to_string()));
        }
        self.validate_entry = commands::validation_cmds::is_typing_command(id);
        let r = (cmd.spec.run)(self, params);
        self.validate_entry = false;
        settings::record_command(id, r.is_ok());
        if r.is_ok()
            && id != "edit.repeat"
            && id != "edit.undo"
            && id != "edit.redo"
            && !id.starts_with("nav.")
            && !id.starts_with("select.")
        {
            self.last_command = Some((id.to_string(), params.clone()));
        }
        r
    }

    /// Run `f` as one undoable step. On error the partial change is rolled back.
    pub fn transact<T>(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Tx<'_>, Selection) -> Result<T, CmdError>,
    ) -> Result<T, CmdError> {
        let sel = self.sel;
        let guard = commands::protect_cmds::Guard::before(&self.wb);
        let mut tx = Tx::new(&mut self.wb);
        let r = f(&mut tx, sel);
        let txn = tx.commit(label);
        match r {
            Ok(v) => {
                if let Some(t) = txn {
                    // Protection: refuse edits to locked cells of protected sheets.
                    if let Some(g) = &guard
                        && let Err(e) = g.check(&mut self.wb, &self.view.review.unlocked, &t)
                    {
                        t.revert(&mut self.wb);
                        return Err(e);
                    }
                    let changed = t.changed_cells();
                    self.dirty = true;
                    match &changed {
                        Some(cells) => self.after_cells_change(cells),
                        None => self.after_change(),
                    }
                    // Data validation on typed entries (after recalc: formulas validate
                    // by result).
                    if self.validate_entry
                        && let Some(cells) = &changed
                    {
                        match commands::validation_cmds::check_entries(self, cells) {
                            Err(e) => {
                                t.revert(&mut self.wb);
                                self.after_cells_change(cells);
                                return Err(e);
                            }
                            Ok(Some(warning)) => self.status = warning,
                            Ok(None) => {}
                        }
                    }
                    self.history.push(t);
                    // Typing next to a table grows it (its own undo step).
                    if self.validate_entry
                        && let Some(cells) = changed
                    {
                        self.validate_entry = false;
                        commands::table_cmds::auto_expand(self, &cells);
                        self.validate_entry = true;
                    }
                }
                Ok(v)
            }
            Err(e) => {
                if let Some(t) = txn {
                    t.revert(&mut self.wb);
                }
                Err(e)
            }
        }
    }

    /// The workbook changed arbitrarily (structural edit, undo/redo): full recalc.
    pub fn after_change(&mut self) {
        if self.wb.calc_mode == CalcMode::Automatic {
            self.recalc();
        } else {
            self.cache.invalidate_deps();
        }
    }

    /// Only `cells` changed: recompute them and their dependents.
    pub fn after_cells_change(&mut self, cells: &[(usize, CellAddr)]) {
        if self.wb.calc_mode == CalcMode::Automatic {
            self.last_recalc = sp_formula::recalc::recalc_dirty(
                &mut self.wb,
                &mut self.cache,
                cells,
                self.recalc_opts,
            );
        } else {
            self.cache.invalidate_deps();
        }
    }

    pub fn select(&mut self, a: CellAddr) {
        self.sel = Selection::at(a);
        self.ensure_visible(a);
    }

    pub fn select_range(&mut self, r: CellRange, active: CellAddr) {
        self.sel = Selection {
            active,
            anchor: r.start,
            cursor: r.end,
        };
        self.ensure_visible(active);
    }

    /// Scroll so `a` is inside the visible page.
    pub fn ensure_visible(&mut self, a: CellAddr) {
        let v = &mut self.view;
        let (pr, pc) = (v.page_rows.max(1), v.page_cols.max(1));
        if a.row < v.scroll.row {
            v.scroll.row = a.row;
        } else if a.row >= v.scroll.row + pr {
            v.scroll.row = a.row + 1 - pr;
        }
        if a.col < v.scroll.col {
            v.scroll.col = a.col;
        } else if a.col >= v.scroll.col + pc {
            v.scroll.col = a.col + 1 - pc;
        }
    }

    pub fn title(&self) -> String {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("Book1");
        format!("{}{} - SheetPro", name, if self.dirty { " •" } else { "" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_registered_command_has_unique_id() {
        let mut ids: Vec<&str> = registry().all().iter().map(|c| c.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }

    #[test]
    fn set_formula_undo_redo() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "2"}))
            .unwrap();
        s.execute("cell.set", &json!({"cell": "A2", "input": "=A1*21"}))
            .unwrap();
        let v = |s: &Session, a: &str| s.wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone();
        assert_eq!(v(&s, "A2"), sp_core::Value::Number(42.0));
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.wb.sheets[0].get(CellAddr::parse("A2").unwrap()).is_none());
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(v(&s, "A2"), sp_core::Value::Number(42.0));
        assert!(s.execute("no.such.command", &json!({})).is_err());
    }

    #[test]
    fn cell_edits_recalc_incrementally() {
        let mut s = Session::new();
        let v = |s: &Session, a: &str| s.wb.sheets[0].value(CellAddr::parse(a).unwrap()).clone();
        for (cell, input) in [
            ("A1", "2"),
            ("A2", "=A1*10"),
            ("B3", "=SUM(A:A)"),
            ("B1", "7"),
        ] {
            s.execute("cell.set", &json!({"cell": cell, "input": input}))
                .unwrap();
        }
        assert!(s.last_recalc.incremental);
        assert_eq!(s.last_recalc.formulas, 0); // B1 feeds nothing; used range unchanged
        s.execute("cell.set", &json!({"cell": "A1", "input": "3"}))
            .unwrap();
        assert!(s.last_recalc.incremental);
        assert_eq!(s.last_recalc.formulas, 2);
        assert_eq!(v(&s, "B3"), sp_core::Value::Number(33.0));
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(!s.last_recalc.incremental);
        assert_eq!(v(&s, "B3"), sp_core::Value::Number(22.0));
        s.execute("formulas.calc_manual", &json!({})).unwrap();
        s.execute("cell.set", &json!({"cell": "A1", "input": "4"}))
            .unwrap();
        assert_eq!(v(&s, "B3"), sp_core::Value::Number(22.0));
        s.execute("formulas.calc_automatic", &json!({})).unwrap();
        assert_eq!(v(&s, "B3"), sp_core::Value::Number(44.0));
        s.execute("cell.set", &json!({"cell": "A1", "input": "5"}))
            .unwrap();
        assert!(s.last_recalc.incremental);
        assert_eq!(v(&s, "B3"), sp_core::Value::Number(55.0));
    }
}
