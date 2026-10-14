//! Benchmarks of heavy operations at realistic sizes against perf/budgets.toml.
//! Writes perf/baseline.json (read by the scorecard).

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sp_core::{CellAddr, Value};
use sp_model::{Cell, Workbook};
use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Deserialize)]
struct Budgets {
    bench: BTreeMap<String, Budget>,
}

#[derive(Deserialize)]
struct Budget {
    ms: f64,
    description: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Baseline {
    pub date: String,
    pub quick: bool,
    pub results: BTreeMap<String, (f64, f64, String)>,
}

fn time<T>(f: impl FnOnce() -> T) -> (f64, T) {
    let t = Instant::now();
    let r = f();
    (t.elapsed().as_secs_f64() * 1000.0, r)
}

fn numbers_and_formulas(rows: u32) -> Workbook {
    let mut wb = Workbook::new();
    let s = &mut wb.sheets[0];
    for r in 0..rows {
        s.set(
            CellAddr::new(r, 0),
            Cell::constant(Value::Number(f64::from(r))),
        );
        s.set(
            CellAddr::new(r, 1),
            Cell::constant(Value::text(format!("item {r}"))),
        );
        s.set(
            CellAddr::new(r, 2),
            Cell::formula(format!("A{}*2+1", r + 1)),
        );
        if r > 0 {
            s.set(
                CellAddr::new(r, 3),
                Cell::formula(format!("D{}+C{}", r, r + 1)),
            );
        }
    }
    wb
}

/// `rows` × 10 columns: text, numbers and dates, like a sales report.
fn report(rows: u32) -> Workbook {
    let mut wb = Workbook::new();
    let s = &mut wb.sheets[0];
    for r in 0..rows {
        let f = f64::from(r);
        for c in 0..10u32 {
            let v = match c {
                0 => Value::text(format!("Region {}", r % 17)),
                1 => Value::text(format!("Product {r}")),
                5 | 9 => Value::text(format!("note {}", r % 101)),
                _ => Value::Number(f * f64::from(c) + 0.25),
            };
            s.set(CellAddr::new(r, c), Cell::constant(v));
        }
    }
    wb
}

pub fn run(quick: bool) -> Result<()> {
    let budgets: Budgets = toml::from_str(&std::fs::read_to_string(
        crate::root().join("perf/budgets.toml"),
    )?)?;
    let n: u32 = if quick { 20_000 } else { 100_000 };
    let mut results: BTreeMap<String, f64> = BTreeMap::new();

    let mut wb = numbers_and_formulas(n);
    let mut cache = sp_formula::AstCache::default();
    let (ms, stats) =
        time(|| sp_formula::recalc(&mut wb, &mut cache, sp_formula::RecalcOptions::default()));
    results.insert("recalc_full_cold".into(), ms);
    let (ms, _) =
        time(|| sp_formula::recalc(&mut wb, &mut cache, sp_formula::RecalcOptions::default()));
    results.insert("recalc_full_warm".into(), ms);
    println!(
        "  ({} formulas, {} evaluations)",
        stats.formulas, stats.evaluations
    );

    // Single edit in a big sheet, through the engine (includes undo recording + recalc).
    let mut session = sp_engine::Session::with_workbook(wb.clone());
    let (ms, r) = time(|| session.execute("cell.set", &json!({"cell": "A1", "input": "42"})));
    r.map_err(|e| anyhow::anyhow!("{e}"))?;
    results.insert("edit_one_cell".into(), ms);

    let mut buf = std::io::Cursor::new(Vec::new());
    let (ms, r) = time(|| sp_io::xlsx_write::write_xlsx(&wb, &mut buf));
    r?;
    results.insert("xlsx_save".into(), ms);
    buf.set_position(0);
    let (ms, r) = time(|| sp_io::xlsx_read::read_xlsx(buf));
    r?;
    results.insert("xlsx_load".into(), ms);

    // Grid layout of one screen deep in a large sheet.
    let sheet = wb.sheets[0].clone();
    let (ms, _) = time(|| {
        sp_render::layout(
            &wb,
            &sheet,
            CellAddr::new(n / 2, 0),
            1600.0,
            1000.0,
            1.0,
            false,
        )
    });
    results.insert("grid_layout_screen".into(), ms);

    let (ms, _) = time(|| {
        let mut s = sp_engine::Session::with_workbook(wb.clone());
        let _ = s.execute(
            "data.sort_za",
            &json!({"range": format!("A1:D{n}"), "header": false}),
        );
    });
    results.insert("sort_rows".into(), ms);

    // PDF export of a 10-column report (paginate + lay out + write), through the engine.
    let pdf_rows = n / 10;
    let mut s = sp_engine::Session::with_workbook(report(pdf_rows));
    s.execute(
        "home.number.format",
        &json!({"range": format!("C1:D{pdf_rows}"), "code": "$#,##0.00"}),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let pdf = std::env::temp_dir().join(format!("sp_perf_{}.pdf", std::process::id()));
    let (ms, r) = time(|| {
        s.execute(
            "file.export.pdf",
            &json!({"path": pdf.display().to_string()}),
        )
    });
    let out = r.map_err(|e| anyhow::anyhow!("{e}"))?;
    let _ = std::fs::remove_file(&pdf);
    println!("  ({pdf_rows}x10 report → {} PDF pages)", out["pages"]);
    results.insert("pdf_export".into(), ms);

    let mut base = Baseline {
        date: crate::corpus::today(),
        quick,
        results: BTreeMap::new(),
    };
    let mut over = Vec::new();
    println!("perf ({} rows{}):", n, if quick { ", quick" } else { "" });
    for (k, ms) in &results {
        let (budget, desc) = budgets
            .bench
            .get(k)
            .map_or((f64::INFINITY, String::new()), |b| {
                (b.ms, b.description.clone())
            });
        // Quick mode runs 1/5 of the size; scale budgets accordingly.
        let budget = if quick { budget / 5.0 } else { budget };
        let flag = if *ms > budget { "OVER" } else { "ok" };
        println!("  {k:<22} {ms:>9.1} ms  budget {budget:>8.1} ms  {flag}");
        if *ms > budget {
            over.push(k.clone());
        }
        base.results.insert(k.clone(), (*ms, budget, desc));
    }
    if !quick {
        std::fs::write(
            crate::root().join("perf/baseline.json"),
            serde_json::to_string_pretty(&base)?,
        )?;
    }
    if !over.is_empty() {
        bail!("over budget: {}", over.join(", "));
    }
    Ok(())
}
