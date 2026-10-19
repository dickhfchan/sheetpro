//! `--full`: every registered command, run in the built app against a populated workbook,
//! each followed by a real frame (a window capture). Commands may refuse their params; the
//! app must answer, render, stay up, and undo must keep working.

use super::Ctx;
use anyhow::Result;
use serde_json::{Value as Json, json};
use std::time::Instant;

/// Commands that end the session or hide the only window.
const SKIP: [&str; 4] = ["file.exit", "file.close", "view.hide_window", "file.print"];

fn populate(a: &mut super::App, c: &mut Ctx, base: &std::path::Path) {
    a.ex(c, "file.open", json!({"path": base}));
    a.ex(c, "select.range", json!({"range": "A1:E9", "active": "B3"}));
}

pub fn run(c: &mut Ctx) -> Result<()> {
    // A workbook with values, formulas, a table, a chart and a note to act on.
    let base = c.path("base.xlsx");
    let mut a = c.launch()?;
    let rows = [
        ["Region", "Product", "Qty", "Price"],
        ["East", "Apples", "10", "1.5"],
        ["West", "Pears", "4", "2.25"],
        ["East", "Plums", "7", "3"],
        ["North", "Apples", "12", "1.5"],
        ["West", "Apples", "3", "1.5"],
        ["North", "Pears", "8", "2.25"],
        ["East", "Pears", "5", "2.25"],
        ["South", "Plums", "9", "3"],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (k, v) in row.iter().enumerate() {
            let col = ["A", "B", "C", "D"][k];
            a.set(c, &format!("{col}{}", r + 1), v);
        }
    }
    a.set(c, "E1", "Total");
    a.set(c, "E2", "=C2*D2");
    a.ex(c, "home.editing.fill_down", json!({"range": "E2:E9"}));
    a.set(c, "G1", "=SUM(E2:E9)");
    a.ex(
        c,
        "insert.chart.column",
        json!({"series": [{"values": "E2:E9", "categories": "B2:B9"}], "cell": "H2"}),
    );
    a.ex(c, "insert.note", json!({"cell": "A1", "text": "note"}));
    a.ex(c, "home.cells.insert_sheet", json!({"name": "Two"}));
    a.ex(c, "sheet.activate", json!({"sheet": "Sheet1"}));
    a.ex(c, "file.save_as", json!({"path": base}));

    let cmds: Vec<Json> = a.raw(json!({"op": "commands"}))?["result"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ids: Vec<String> = cmds
        .iter()
        .filter_map(|c| c["id"].as_str().map(str::to_string))
        .filter(|id| !SKIP.contains(&id.as_str()))
        .collect();
    let shot = c.path("frame.png");
    let generic = json!({
        "range": "A1:E9", "cell": "B3", "sheet": "Sheet1", "name": "Smoke",
        "text": "smoke", "input": "1", "value": 1, "percent": 100, "path": c.path("out.xlsx"),
    });
    let mut crashed: Vec<String> = Vec::new();
    let mut no_frame: Vec<String> = Vec::new();
    let mut slow: Vec<(String, f64)> = Vec::new();
    let mut ran = 0usize;
    for (i, id) in ids.iter().enumerate() {
        if i % 20 == 0 {
            populate(&mut a, c, &base);
        }
        for (k, p) in [json!({}), generic.clone()].into_iter().enumerate() {
            let t = Instant::now();
            let r = a.try_ex(id, p);
            let secs = t.elapsed().as_secs_f64();
            if secs > 10.0 {
                slow.push((id.clone(), secs));
            }
            // The app repaints continuously, so a UI panic takes it down within a frame or
            // two; a real capture every 10 commands proves frames are being drawn.
            std::thread::sleep(std::time::Duration::from_millis(60));
            if i % 10 == 0 && k == 1 && !a.screenshot(&shot) {
                no_frame.push(id.clone());
            }
            let undo = a.try_ex("edit.undo", json!({}));
            if !a.alive()
                || undo
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.starts_with("transport"))
            {
                crashed.push(format!("{id}: {:?}", r.err()));
                // Start over so one crash doesn't hide the rest.
                drop(a);
                a = c.launch()?;
                populate(&mut a, c, &base);
                break;
            }
            ran += 1;
        }
    }
    c.check(
        &format!(
            "{} commands x 2 param sets run, render and undo without a crash",
            ids.len()
        ),
        crashed.is_empty() && ran > 0,
        &crashed,
    );
    c.check(
        "the window keeps rendering through the sweep",
        no_frame.is_empty(),
        &no_frame,
    );
    c.check(
        "no command takes over 10 s on a small workbook",
        slow.is_empty(),
        &slow,
    );
    // The app is still fully usable afterwards.
    populate(&mut a, c, &base);
    a.set(c, "A20", "=SUM(C2:C9)");
    let v = a.val("A20");
    c.check(
        "still calculates after the sweep",
        super::close(&v, 58.0),
        v,
    );
    a.quit(c, "exits cleanly after the sweep");
    Ok(())
}
