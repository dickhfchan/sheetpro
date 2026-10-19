//! `--full`: a 100k-row workbook in the built app, with generous wall-clock budgets (the
//! release app, a real window, a loaded machine). `cargo xtask perf` has the tight ones.

use super::{Ctx, close};
use anyhow::{Context, Result};
use serde_json::json;
use sp_core::{CellAddr, Value};
use sp_model::{Cell, Workbook};
use std::time::Instant;

const ROWS: u32 = 100_000;

pub fn run(c: &mut Ctx) -> Result<()> {
    let path = c.path("big.xlsx");
    let mut wb = Workbook::new();
    let sh = wb.sheets.first_mut().context("no sheet")?;
    for r in 0..ROWS {
        sh.set(
            CellAddr::new(r, 0),
            Cell::constant(Value::Number(f64::from(r % 997))),
        );
        sh.set(
            CellAddr::new(r, 1),
            Cell::constant(Value::text(format!("item {}", r % 113))),
        );
        sh.set(CellAddr::new(r, 2), Cell::formula(format!("A{}*2", r + 1)));
    }
    sh.set(
        CellAddr::new(0, 4),
        Cell::formula(format!("SUM(C1:C{ROWS})")),
    );
    sp_io::save(&wb, &path).context("writing the 100k-row workbook")?;
    let want: f64 = (0..ROWS).map(|r| f64::from(r % 997) * 2.0).sum();

    let mut a = c.launch()?;
    let timed = |c: &mut Ctx, what: &str, budget: f64, a: &mut super::App, id: &str, p| {
        let t = Instant::now();
        let r = a.try_ex(id, p);
        let secs = t.elapsed().as_secs_f64();
        c.check(
            &format!("{what} under {budget} s"),
            r.is_ok() && secs < budget,
            (secs, r.err()),
        );
    };
    timed(
        c,
        "open 100k rows",
        15.0,
        &mut a,
        "file.open",
        json!({"path": path}),
    );
    let v = a.val("E1");
    c.check("the total is right after load", close(&v, want), v);
    timed(
        c,
        "edit with 100k dependents",
        3.0,
        &mut a,
        "cell.set",
        json!({"cell": "A1", "input": "1000"}),
    );
    let v = a.val("E1");
    c.check(
        "the edit recalculates the total",
        close(&v, want + 2000.0),
        v,
    );
    timed(
        c,
        "jump to the end",
        3.0,
        &mut a,
        "nav.end_sheet",
        json!({}),
    );
    let t = Instant::now();
    let shot = c.path("end.png");
    let ok = a.screenshot(&shot);
    let secs = t.elapsed().as_secs_f64();
    c.check("render at row 100k under 5 s", ok && secs < 5.0, secs);
    timed(
        c,
        "sort 100k rows",
        20.0,
        &mut a,
        "data.sort",
        json!({"range": format!("A1:C{ROWS}"), "keys": [{"column": "B"}]}),
    );
    timed(
        c,
        "filter 100k rows",
        10.0,
        &mut a,
        "data.filter",
        json!({"range": format!("A1:C{ROWS}"), "on": true, "column": "B", "values": ["item 7"]}),
    );
    timed(
        c,
        "clear the filter",
        10.0,
        &mut a,
        "data.filter_clear",
        json!({}),
    );
    timed(
        c,
        "find all in 100k rows",
        10.0,
        &mut a,
        "edit.find_all",
        json!({"what": "item 42", "whole": true}),
    );
    timed(c, "undo the sort", 20.0, &mut a, "edit.undo", json!({}));
    let saved = c.path("big-saved.xlsx");
    timed(
        c,
        "save 100k rows",
        20.0,
        &mut a,
        "file.save_as",
        json!({"path": saved}),
    );
    a.quit(c, "exits cleanly with a big workbook");
    Ok(())
}
