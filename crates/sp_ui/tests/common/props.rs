//! The property-test runner (see `tests/properties.rs` for what it checks).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
use super::*;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value as Json, json};
use sp_core::{CellAddr, CellRange};
use sp_engine::props::{Change, PropMap, diff, schema_path, snapshot};
use sp_engine::{Session, settings};
use sp_ui::App;
use std::collections::BTreeSet;

/// A step of a custom action (see `Act::Custom`).
pub type Step = fn(&mut Harness<'_, App>) -> Result<(), String>;
/// An extra check on the session after the action (see `Entry::then`).
pub type Check = fn(&Session) -> Result<(), String>;

/// How the user does it.
pub enum Act {
    /// A ribbon button: tab, group, label.
    Ribbon(&'static str, &'static str, &'static str),
    /// A ribbon menu (tab, group, label), then the option chosen in it.
    Pick(&'static str, &'static str, &'static str, &'static str),
    /// A keyboard shortcut, e.g. `Ctrl+B`.
    Key(&'static str),
    /// Click a cell, type, press Enter.
    Type(&'static str, &'static str),
    /// Open a dialog from the ribbon, fill it in (`fill` edits the open dialog the way the
    /// user's typing would), then click `ok`.
    Dialog {
        ribbon: (&'static str, &'static str, &'static str),
        fill: fn(&mut App),
        ok: &'static str,
    },
    /// Anything else a user does (right-click menus, dialogs opened by a key, mouse drags):
    /// `prep` navigates (not part of the change, e.g. a tab click), the snapshot is taken,
    /// then `act` does the action.
    Custom { prep: Step, act: Step },
}

pub struct Entry {
    pub name: &'static str,
    /// Commands run on the base workbook first.
    pub setup: Vec<(&'static str, Json)>,
    /// The selection the action starts from (active cell = its top-left).
    pub select: &'static str,
    pub act: Act,
    /// Exact new value per changed path (`None` = the property is gone).
    pub expect: Vec<(String, Option<String>)>,
    /// Further changes allowed without pinning their values (glob, `*` = any run).
    pub allow: Vec<&'static str>,
    /// Undoable (view changes such as zoom are not).
    pub undo: bool,
    /// Checked after save and reopen (a few properties are not stored in .xlsx).
    pub persist: bool,
    /// Also export a PDF (page and visual properties).
    pub print: bool,
    /// Extra assertions on the session after the action (selection, status, things that
    /// are not saved properties).
    pub then: Option<Check>,
}

impl Entry {
    pub fn new(name: &'static str, select: &'static str, act: Act) -> Self {
        Entry {
            name,
            setup: Vec::new(),
            select,
            act,
            expect: Vec::new(),
            allow: Vec::new(),
            undo: true,
            persist: true,
            print: false,
            then: None,
        }
    }
    pub fn setup(mut self, id: &'static str, p: Json) -> Self {
        self.setup.push((id, p));
        self
    }
    /// `path` becomes `value` (as the snapshot prints it: strings quoted, numbers as JSON).
    pub fn set(mut self, path: &str, value: &str) -> Self {
        self.expect
            .push((path.to_string(), Some(value.to_string())));
        self
    }
    pub fn gone(mut self, path: &str) -> Self {
        self.expect.push((path.to_string(), None));
        self
    }
    /// The same property on every cell of `range` (`Sheet1!A2:B3`).
    pub fn cells(mut self, range: &str, prop: &str, value: &str) -> Self {
        let (sheet, r) = range.split_once('!').unwrap();
        let r = CellRange::parse(r).unwrap();
        for a in r.iter() {
            self.expect.push((
                format!("{sheet}!{}.{prop}", a.to_a1()),
                Some(value.to_string()),
            ));
        }
        self
    }
    pub fn allow(mut self, glob: &'static str) -> Self {
        self.allow.push(glob);
        self
    }
    pub fn view_only(mut self) -> Self {
        self.undo = false;
        self.persist = false;
        self
    }
    pub fn then(mut self, f: Check) -> Self {
        self.then = Some(f);
        self
    }
    pub fn print(mut self) -> Self {
        self.print = true;
        self
    }
}

pub fn glob(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((a, rest)) => {
            let Some(tail) = s.strip_prefix(a) else {
                return false;
            };
            (0..=tail.len()).any(|i| tail.is_char_boundary(i) && glob(rest, &tail[i..]))
        }
    }
}

pub fn press(h: &mut Harness<'_, App>, sc: &str) -> Result<(), String> {
    let (m, k) = key_for(sc).ok_or_else(|| format!("no key for {sc}"))?;
    h.key_press_modifiers(m, k);
    h.run_steps(3);
    Ok(())
}

pub fn click_cell(h: &mut Harness<'_, App>, a1: &str) {
    let p = cell_center(h, a1);
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(2);
}

/// Perform the action; the snapshot is taken after navigating (tab switch) and before the
/// click that changes the workbook.
pub fn perform(h: &mut Harness<'_, App>, act: &Act) -> Result<PropMap, String> {
    let snap = |h: &Harness<'_, App>| snapshot(&h.state().session);
    match act {
        Act::Ribbon(tab, group, label) => {
            click_tab(h, tab)?;
            let before = snap(h);
            click_in_ribbon(h, group, label)?;
            h.run_steps(3);
            Ok(before)
        }
        Act::Pick(tab, group, label, option) => {
            click_tab(h, tab)?;
            let before = snap(h);
            click_in_ribbon(h, group, label)?;
            h.run_steps(2);
            click_new(h, option, &[])?;
            Ok(before)
        }
        Act::Key(sc) => {
            let before = snap(h);
            press(h, sc)?;
            Ok(before)
        }
        Act::Type(cell, text) => {
            click_cell(h, cell);
            let before = snap(h);
            h.event(egui::Event::Text((*text).to_string()));
            h.run_steps(1);
            press(h, "Enter")?;
            Ok(before)
        }
        Act::Dialog { ribbon, fill, ok } => {
            click_tab(h, ribbon.0)?;
            let before = snap(h);
            click_in_ribbon(h, ribbon.1, ribbon.2)?;
            h.run_steps(3);
            if h.state().dialog_kind().is_none() {
                return Err("the dialog did not open".into());
            }
            fill(h.state_mut());
            h.run_steps(2);
            h.get_by_label(ok).click();
            h.run_steps(3);
            if let Some(k) = h.state().dialog_kind() {
                return Err(format!(
                    "dialog '{k}' stayed open: {:?}",
                    h.state().last_error()
                ));
            }
            Ok(before)
        }
        Act::Custom { prep, act } => {
            prep(h)?;
            let before = snap(h);
            act(h)?;
            h.run_steps(3);
            Ok(before)
        }
    }
}

pub fn show(changes: &[Change]) -> String {
    changes
        .iter()
        .map(|c| format!("\n      {c}"))
        .collect::<String>()
}

/// What one entry proved, for the coverage report.
#[derive(Default)]
pub struct Proved {
    pub commands: BTreeSet<String>,
    pub changed: BTreeSet<String>,
    pub persisted: BTreeSet<String>,
}

pub fn run(e: &Entry, proved: &mut Proved) -> Result<(), String> {
    let mut s = base_session();
    for (id, p) in &e.setup {
        s.execute(id, p).map_err(|x| format!("setup {id}: {x}"))?;
    }
    let r = CellRange::parse(e.select).map_err(|_| "bad selection")?;
    s.select_range(r, r.start);
    // Undo in the checks may only reach the action under test.
    s.history.clear();
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(WIDTH, 900.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), App::new(s));
    h.run_steps(3);
    let log_before = settings::command_log();
    let before = perform(&mut h, &e.act)?;
    let after = snapshot(&h.state().session);
    proved.commands.extend(
        new_log(&log_before)
            .into_iter()
            .filter(|l| !l.ends_with("(failed)")),
    );

    if let Some(f) = e.then {
        f(&h.state().session)?;
    }

    // 1. Exactly the expected changes. A path with `*` covers every changed path it matches
    // (at least one), all of which must end at the given value.
    let changes = diff(&before, &after);
    let mut problems = Vec::new();
    for (path, want) in &e.expect {
        if path.contains('*') {
            let hit: Vec<&Change> = changes.iter().filter(|c| glob(path, &c.path)).collect();
            if hit.is_empty() {
                problems.push(format!("{path}: nothing matching changed"));
            }
            for c in hit {
                if c.after.as_ref() != want.as_ref() {
                    problems.push(format!("{}: expected {want:?}, got {:?}", c.path, c.after));
                }
            }
            continue;
        }
        let got = after.get(path);
        if got != want.as_ref() {
            problems.push(format!("{path}: expected {want:?}, got {got:?}"));
        }
    }
    let unexpected: Vec<Change> = changes
        .iter()
        .filter(|c| {
            !e.expect
                .iter()
                .any(|(p, _)| *p == c.path || glob(p, &c.path))
        })
        .filter(|c| !e.allow.iter().any(|g| glob(g, &c.path)))
        .cloned()
        .collect();
    if !unexpected.is_empty() {
        problems.push(format!("unexpected changes:{}", show(&unexpected)));
    }
    if !problems.is_empty() {
        return Err(format!(
            "{}\n    all changes:{}",
            problems.join("\n    "),
            show(&changes)
        ));
    }
    let sheets: Vec<String> = h
        .state()
        .session
        .wb
        .sheets
        .iter()
        .map(|s| s.name.clone())
        .collect();
    proved
        .changed
        .extend(changes.iter().map(|c| schema_path(&c.path, &sheets)));

    // 2. Undo restores everything, redo reapplies it (through Ctrl+Z / Ctrl+Y).
    if e.undo {
        press(&mut h, "Ctrl+Z")?;
        let undone = diff(&before, &snapshot(&h.state().session));
        if !undone.is_empty() {
            return Err(format!("undo left changes:{}", show(&undone)));
        }
        press(&mut h, "Ctrl+Y")?;
        let redone = diff(&after, &snapshot(&h.state().session));
        if !redone.is_empty() {
            return Err(format!("redo differs:{}", show(&redone)));
        }
    }

    // 3. Saved and reopened, the changed properties are still there.
    if e.persist {
        let mut buf = std::io::Cursor::new(Vec::new());
        sp_io::xlsx_write::write_xlsx(&h.state().session.wb, &mut buf)
            .map_err(|x| format!("save: {x}"))?;
        let bytes = buf.into_inner();
        let invalid = sp_io::xlsx_passthrough::validate_package(&bytes);
        if !invalid.is_empty() {
            return Err(format!("saved package invalid: {invalid:?}"));
        }
        let (wb, _) = sp_io::xlsx_read::read_xlsx(std::io::Cursor::new(bytes))
            .map_err(|x| format!("reopen: {x}"))?;
        let mut s2 = Session::new();
        s2.wb = wb;
        let back = snapshot(&s2);
        let lost: Vec<String> = changes
            .iter()
            .filter(|c| !c.path.starts_with("view."))
            .filter(|c| back.get(&c.path) != c.after.as_ref())
            .map(|c| format!("{}: {:?} after reopening", c, back.get(&c.path)))
            .collect();
        if !lost.is_empty() {
            return Err(format!(
                "lost on save/reopen:\n      {}",
                lost.join("\n      ")
            ));
        }
        proved.persisted.extend(
            changes
                .iter()
                .filter(|c| !c.path.starts_with("view."))
                .map(|c| schema_path(&c.path, &sheets)),
        );
    }

    // 4. The shell still renders the result.
    h.run_steps(3);

    // 5. Page and visual properties print.
    if e.print {
        let pdf = std::env::temp_dir().join(format!(
            "sp-props-{}-{}.pdf",
            std::process::id(),
            e.name.replace(' ', "_")
        ));
        h.state_mut()
            .try_exec("file.export.pdf", json!({ "path": pdf }))
            .map_err(|x| format!("pdf: {x}"))?;
        let data = std::fs::read(&pdf).unwrap_or_default();
        if !data.starts_with(b"%PDF-") {
            return Err("pdf not written".into());
        }
    }
    Ok(())
}

/// Values, formulas and a little structure to act on (Sheet1 A1:E6, a second sheet).
pub fn base_session() -> Session {
    let mut s = Session::new();
    let rows = [
        ["Region", "Product", "Qty", "Price"],
        ["East", "Apples", "10", "1.5"],
        ["West", "Pears", "4", "2.25"],
        ["East", "Plums", "7", "3"],
        ["North", "Apples", "12", "1.5"],
        ["West", "Apples", "3", "1.5"],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            let a = CellAddr::new(r as u32, c as u32).to_a1();
            s.execute("cell.set", &json!({"cell": a, "input": v}))
                .unwrap();
        }
    }
    s.execute("cell.set", &json!({"cell": "E1", "input": "Total"}))
        .unwrap();
    s.execute("cell.set", &json!({"cell": "E2", "input": "=C2*D2"}))
        .unwrap();
    s.execute("home.editing.fill_down", &json!({"range": "E2:E6"}))
        .unwrap();
    s.execute("cell.set", &json!({"cell": "G1", "input": "=SUM(E2:E6)"}))
        .unwrap();
    s
}

/// Run every entry, print a table and the coverage, and return what was proved and which
/// entries failed (their names).
pub fn check_all(entries: &[Entry]) -> (Proved, Vec<&'static str>) {
    let mut proved = Proved::default();
    let mut failed = Vec::new();
    for e in entries {
        match run(e, &mut proved) {
            Ok(()) => println!("ok    {}", e.name),
            Err(why) => {
                println!("FAIL  {}\n    {why}", e.name);
                failed.push(e.name);
            }
        }
    }
    println!(
        "\nproperties: {} of {} entries pass; {} commands proved; {} property kinds changed, {} round-tripped",
        entries.len() - failed.len(),
        entries.len(),
        proved.commands.len(),
        proved.changed.len(),
        proved.persisted.len()
    );
    for p in &proved.changed {
        let saved = if proved.persisted.contains(p) {
            ""
        } else {
            "   (not round-tripped)"
        };
        println!("  {p}{saved}");
    }
    (proved, failed)
}
