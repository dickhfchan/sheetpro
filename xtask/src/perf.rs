//! Benchmarks of heavy operations at realistic sizes against perf/budgets.toml.
//! Writes perf/baseline.json (read by the scorecard).
//!
//! - `cargo xtask perf --quick`: the base suite at 20k rows (budgets / 5). Fast enough for CI.
//! - `cargo xtask perf`: the base suite at 100k rows plus the 1M-row suite (`*_1m` benches:
//!   1M rows x 10 mixed columns, 4M formulas), and writes the baseline.
//! - `cargo xtask perf --million`: only the 1M-row suite (for profiling; writes nothing).

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
fn sales_report(rows: u32) -> Workbook {
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

pub fn run(quick: bool, million_only: bool) -> Result<()> {
    let budgets: Budgets = toml::from_str(&std::fs::read_to_string(
        crate::root().join("perf/budgets.toml"),
    )?)?;
    let n: u32 = if quick { 20_000 } else { 100_000 };
    let mut results: BTreeMap<String, f64> = BTreeMap::new();
    if !million_only {
        base_suite(n, &mut results)?;
    }
    if !quick {
        million::suite(&mut results)?;
    }
    report(&budgets, quick, million_only, n, &results)
}

/// The base suite at `n` rows (4 columns: number, text, 2 formulas).
fn base_suite(n: u32, results: &mut BTreeMap<String, f64>) -> Result<()> {
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
    let mut s = sp_engine::Session::with_workbook(sales_report(pdf_rows));
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
    Ok(())
}

fn report(
    budgets: &Budgets,
    quick: bool,
    million_only: bool,
    n: u32,
    results: &BTreeMap<String, f64>,
) -> Result<()> {
    let mut base = Baseline {
        date: crate::corpus::today(),
        quick,
        results: BTreeMap::new(),
    };
    let mut over = Vec::new();
    println!(
        "perf ({} rows{}):",
        if million_only {
            "1M".to_string()
        } else {
            n.to_string()
        },
        if quick { ", quick" } else { "" }
    );
    for (k, ms) in results {
        let (budget, desc) = budgets
            .bench
            .get(k)
            .map_or((f64::INFINITY, String::new()), |b| {
                (b.ms, b.description.clone())
            });
        // Quick mode runs 1/5 of the size; scale budgets accordingly.
        let budget = if quick { budget / 5.0 } else { budget };
        let flag = if *ms > budget { "OVER" } else { "ok" };
        let unit = unit_of(k);
        println!("  {k:<26} {ms:>9.1} {unit}  budget {budget:>8.1} {unit}  {flag}");
        if *ms > budget {
            over.push(k.clone());
        }
        base.results.insert(k.clone(), (*ms, budget, desc));
    }
    if !quick && !million_only {
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

/// Display unit of a bench: `*_mb` benches measure memory, the rest time.
pub fn unit_of(key: &str) -> &'static str {
    if key.ends_with("_mb") { "MB" } else { "ms" }
}

/// 1M rows x 10 columns of mixed values and formulas.
mod million {
    use super::time;
    use anyhow::{Result, anyhow, bail};
    use serde_json::json;
    use sp_core::{CellAddr, Value};
    use sp_engine::Session;
    use sp_model::{Cell, Workbook};
    use std::collections::BTreeMap;

    pub const ROWS: u32 = 1_000_000;
    /// The row the "deep in the sheet" screen benches look at.
    const DEEP: u32 = 900_000;

    /// Columns: A number, B unique text, C `=A*2+1`, D category text (17 values), E number,
    /// F date serial, G bool, H `=IF(E>500,"hi","lo")`, I `=C+E`, J running total `=J(n-1)+I`.
    /// Four formula columns: 4M formulas, one of them a 1M-long dependency chain.
    pub fn workbook() -> Workbook {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        for r in 0..ROWS {
            let n = r + 1;
            let f = f64::from(r);
            let cells = [
                Cell::constant(Value::Number(f)),
                Cell::constant(Value::text(format!("item {r}"))),
                Cell::formula(format!("A{n}*2+1")),
                Cell::constant(Value::text(format!("Region {}", r % 17))),
                Cell::constant(Value::Number(f64::from((r * 7919) % 1000) + 0.5)),
                Cell::constant(Value::Number(45_000.0 + f64::from(r % 3650))),
                Cell::constant(Value::Bool(r % 3 == 0)),
                Cell::formula(format!("IF(E{n}>500,\"hi\",\"lo\")")),
                Cell::formula(format!("C{n}+E{n}")),
                if r == 0 {
                    Cell::formula("I1")
                } else {
                    Cell::formula(format!("J{r}+I{n}"))
                },
            ];
            for (c, cell) in cells.into_iter().enumerate() {
                s.set(CellAddr::new(r, c as u32), cell);
            }
        }
        wb
    }

    /// Peak resident set size of this process so far, in MB.
    #[allow(unsafe_code)]
    pub fn peak_rss_mb() -> f64 {
        let mut ru = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage writes a `rusage` into the provided, properly sized buffer.
        let ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, ru.as_mut_ptr()) } == 0;
        if !ok {
            return 0.0;
        }
        // SAFETY: zero-initialised and filled by a successful getrusage.
        let max = unsafe { ru.assume_init() }.ru_maxrss as f64;
        // macOS reports bytes, Linux kilobytes.
        if cfg!(target_os = "macos") {
            max / (1024.0 * 1024.0)
        } else {
            max / 1024.0
        }
    }

    fn exec(s: &mut Session, id: &str, p: serde_json::Value) -> Result<serde_json::Value> {
        s.execute(id, &p).map_err(|e| anyhow!("{id}: {e}"))
    }

    fn layout(s: &Session, top: CellAddr) -> sp_render::GridView {
        let sh = &s.wb.sheets[0];
        sp_render::layout(&s.wb, sh, top, 1600.0, 1000.0, 1.0, false)
    }

    fn expect_value(s: &Session, a1: &str, want: &Value) -> Result<()> {
        let a = CellAddr::parse(a1)?;
        let got = s.wb.sheets[0].value(a);
        if got != want {
            bail!("1M suite: {a1} is {got:?}, expected {want:?}");
        }
        Ok(())
    }

    pub fn suite(results: &mut BTreeMap<String, f64>) -> Result<()> {
        let mut put = |k: &str, v: f64| {
            println!("  {k:<26} {v:>9.1} {}", super::unit_of(k));
            results.insert(k.to_string(), v);
        };
        println!("1M-row suite ({ROWS} rows x 10 cols):");
        let (ms, mut wb) = time(workbook);
        println!("  (build {ms:.0} ms)");

        let mut cache = sp_formula::AstCache::default();
        let opts = sp_formula::RecalcOptions::default();
        let (ms, stats) = time(|| sp_formula::recalc(&mut wb, &mut cache, opts));
        put("recalc_full_cold_1m", ms);
        println!(
            "  ({} formulas, {} evaluations)",
            stats.formulas, stats.evaluations
        );
        let (ms, _) = time(|| sp_formula::recalc(&mut wb, &mut cache, opts));
        put("recalc_full_warm_1m", ms);
        drop(cache);

        let mut buf = std::io::Cursor::new(Vec::new());
        let (ms, r) = time(|| sp_io::xlsx_write::write_xlsx(&wb, &mut buf));
        r?;
        put("xlsx_save_1m", ms);
        let bytes = buf.into_inner();
        println!("  (.xlsx {:.1} MB)", bytes.len() as f64 / 1e6);
        let (ms, r) = time(|| sp_io::xlsx_read::read_xlsx(std::io::Cursor::new(&bytes)));
        let (loaded, _) = r?;
        put("xlsx_load_1m", ms);
        if loaded.sheets.first().map(sp_model::Sheet::cell_count) != Some(10 * ROWS as usize) {
            bail!("1M suite: loaded workbook lost cells");
        }
        drop(loaded);
        drop(bytes);

        let mut s = Session::with_workbook(wb);
        // Running total of I = 2r+1 + E; check the chain end so optimisations can't cheat.
        let last = format!("J{ROWS}");
        let total = s.wb.sheets[0].value(CellAddr::parse(&last)?).clone();
        if !matches!(total, Value::Number(_)) {
            bail!("1M suite: {last} is {total:?}");
        }

        // Screens deep in the sheet.
        let deep = CellAddr::new(DEEP, 0);
        let (ms, v) = time(|| layout(&s, deep));
        if v.cells.is_empty() {
            bail!("1M suite: empty layout at row {DEEP}");
        }
        put("grid_layout_screen_1m", ms);
        // Scrolling: 120 frames, 3 rows per frame, from row 900k. Report the worst frame.
        let mut worst = 0.0f64;
        for i in 0..120 {
            let (ms, _) = time(|| layout(&s, CellAddr::new(DEEP + i * 3, 0)));
            worst = worst.max(ms);
        }
        put("scroll_frame_1m", worst);
        // Ctrl+Down from A1 to the last row, then draw the screen there.
        s.select(CellAddr::new(0, 0));
        let (ms, r) = time(|| {
            let r = exec(&mut s, "nav.edge_down", json!({}));
            let top = s.view.scroll;
            (r, layout(&s, top))
        });
        let at = r.0?;
        if at != json!(format!("A{ROWS}")) {
            bail!("1M suite: Ctrl+Down went to {at}");
        }
        put("jump_end_1m", ms);

        // Edits through the engine (undo record + incremental recalc).
        let (ms, r) = time(|| exec(&mut s, "cell.set", json!({"cell": "G500001", "input": "7"})));
        r?;
        put("edit_one_cell_1m", ms);
        // A2 feeds C2, I2 and the running total J2..J1000000: ~1M dependents.
        let (ms, r) = time(|| exec(&mut s, "cell.set", json!({"cell": "A2", "input": "2"})));
        r?;
        put("edit_chain_1m", ms);
        // A2 went 1 -> 2: C2 = 2*2+1 grew by 2, so every running total from J2 on grew by 2.
        let want = match total.clone() {
            Value::Number(t) => Value::Number(t + 2.0),
            other => other,
        };
        expect_value(&s, &last, &want)?;
        let (ms, r) = time(|| exec(&mut s, "edit.undo", json!({})));
        r?;
        put("undo_edit_chain_1m", ms);
        expect_value(&s, &last, &total)?;

        let (ms, r) = time(|| {
            exec(
                &mut s,
                "edit.find_all",
                json!({"what": "item 99999", "whole": true}),
            )
        });
        let hits = r?;
        if hits.as_array().map(Vec::len) != Some(1) {
            bail!("1M suite: find_all found {hits}");
        }
        put("find_all_1m", ms);

        // AutoFilter: keep one region of 17 (~59k rows shown, ~941k hidden).
        let (ms, r) = time(|| {
            exec(
                &mut s,
                "data.filter",
                json!({"range": format!("A1:J{ROWS}"), "column": "D", "values": ["Region 3"]}),
            )
        });
        let out = r?;
        println!("  (filter shows {} rows)", out["shown"]);
        put("autofilter_apply_1m", ms);
        let (ms, v) = time(|| layout(&s, deep));
        if v.rows.len() < 10 {
            bail!("1M suite: filtered layout has {} rows", v.rows.len());
        }
        put("grid_layout_filtered_1m", ms);
        let (ms, r) = time(|| exec(&mut s, "data.filter", json!({"on": false})));
        r?;
        put("autofilter_clear_1m", ms);

        let (ms, r) = time(|| {
            exec(
                &mut s,
                "data.sort_za",
                json!({"range": format!("A1:J{ROWS}"), "header": true}),
            )
        });
        r?;
        put("sort_rows_1m", ms);

        let (ms, r) = time(|| exec(&mut s, "home.cells.insert_rows", json!({"range": "A2"})));
        r?;
        put("insert_row_1m", ms);

        put("peak_rss_1m_mb", peak_rss_mb());
        Ok(())
    }
}
