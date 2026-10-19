//! `cargo xtask smoke [--bin path]`: drive a built desktop app end to end over its control
//! port (formulas, editing, sort/filter, objects, pivot, save, reopen, PDF) and check the
//! results. `cargo xtask package` runs it on the binary it just bundled; set
//! `SP_SKIP_SMOKE=1` to skip that (e.g. on a machine without a display).

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Checks {
    passed: usize,
    failed: Vec<String>,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
        if ok {
            self.passed += 1;
            println!("PASS {name}");
        } else {
            println!("FAIL {name}  [{detail:?}]");
            self.failed.push(name.to_string());
        }
    }
}

struct App {
    child: Child,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

impl App {
    fn launch(bin: &Path, log: &Path) -> Result<App> {
        let port = free_port()?;
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)?;
        let child = Command::new(bin)
            .args(["--control-port", &port.to_string()])
            .stdout(Stdio::from(out.try_clone()?))
            .stderr(Stdio::from(out))
            .spawn()
            .with_context(|| format!("launching {}", bin.display()))?;
        let start = Instant::now();
        let stream = loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => break s,
                Err(_) if start.elapsed() < Duration::from_secs(30) => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => bail!("the app did not open its control port: {e}"),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        Ok(App {
            child,
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        })
    }

    fn raw(&mut self, msg: Json) -> Result<Json> {
        writeln!(self.writer, "{msg}")?;
        self.writer.flush()?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    }

    /// Run a command; a failure is recorded as a failed check and yields `Null`.
    fn ex(&mut self, c: &mut Checks, id: &str, params: Json) -> Json {
        match self.raw(json!({"op": "exec", "id": id, "params": params})) {
            Ok(r) if r["ok"] == json!(true) => r["result"].clone(),
            Ok(r) => {
                c.check(&format!("command {id} runs"), false, &r["error"]);
                Json::Null
            }
            Err(e) => {
                c.check(&format!("command {id} runs"), false, e.to_string());
                Json::Null
            }
        }
    }

    fn get(&mut self, range: &str) -> Json {
        self.raw(json!({"op": "get", "range": range}))
            .map(|r| r["result"].clone())
            .unwrap_or(Json::Null)
    }

    fn cell(&mut self, cell: &str, key: &str) -> Json {
        self.get(cell)[0][0][key].clone()
    }

    fn quit(mut self, c: &mut Checks, name: &str) {
        let _ = self.raw(json!({"op": "quit"}));
        let start = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(st)) => {
                    c.check(name, st.success(), st);
                    return;
                }
                Ok(None) if start.elapsed() < Duration::from_secs(15) => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => {
                    let _ = self.child.kill();
                    c.check(name, false, "had to kill it");
                    return;
                }
            }
        }
    }
}

fn num(v: &Json) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

fn close(v: &Json, want: f64) -> bool {
    (num(v) - want).abs() < 1e-9
}

fn object_count(v: &Json) -> usize {
    v["objects"].as_array().map_or(0, Vec::len)
}

fn values(grid: &Json) -> Vec<Json> {
    grid.as_array()
        .into_iter()
        .flatten()
        .filter_map(Json::as_array)
        .flatten()
        .map(|c| c["value"].clone())
        .collect()
}

/// The bundled app's binary under `dist/` (what `cargo xtask package` produced).
pub fn packaged_binary(root: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.join("dist/SheetPro.app/Contents/MacOS/sheetpro")
    } else {
        let name = if cfg!(windows) {
            "sheetpro.exe"
        } else {
            "sheetpro"
        };
        let dir = format!(
            "sheetpro-{}-{}-{}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        root.join("dist").join(dir).join(name)
    }
}

pub fn run(bin: &Path) -> Result<()> {
    if !bin.exists() {
        bail!(
            "no app binary at {} (run `cargo xtask package`)",
            bin.display()
        );
    }
    let wd = crate::root().join("target/smoke");
    let _ = std::fs::remove_dir_all(&wd);
    std::fs::create_dir_all(&wd)?;
    let log = wd.join("app.log");
    let pic = wd.join("pic.png");
    let px: Vec<u8> = (0..120u32 * 60)
        .flat_map(|i| [(i % 120 * 2) as u8, (i / 120 * 4) as u8, 180, 255])
        .collect();
    std::fs::write(
        &pic,
        sp_render::objects::encode_png(120, 60, px).context("encoding the test picture")?,
    )?;
    let mut c = Checks {
        passed: 0,
        failed: Vec::new(),
    };
    let mut a = App::launch(bin, &log)?;

    // Data and formulas
    let rows: [(&str, &str, f64, f64); 8] = [
        ("East", "Apples", 10.0, 1.5),
        ("West", "Pears", 4.0, 2.25),
        ("East", "Plums", 7.0, 3.0),
        ("North", "Apples", 12.0, 1.5),
        ("West", "Apples", 3.0, 1.5),
        ("North", "Pears", 8.0, 2.25),
        ("East", "Pears", 5.0, 2.25),
        ("South", "Plums", 9.0, 3.0),
    ];
    for (col, h) in ["A1", "B1", "C1", "D1", "E1"]
        .iter()
        .zip(["Region", "Product", "Qty", "Price", "Total"])
    {
        a.ex(&mut c, "cell.set", json!({"cell": col, "input": h}));
    }
    for (i, (r, p, q, pr)) in rows.iter().enumerate() {
        let n = i + 2;
        for (col, v) in [
            ("A", r.to_string()),
            ("B", p.to_string()),
            ("C", q.to_string()),
            ("D", pr.to_string()),
        ] {
            a.ex(
                &mut c,
                "cell.set",
                json!({"cell": format!("{col}{n}"), "input": v}),
            );
        }
    }
    let total: f64 = rows.iter().map(|r| r.2 * r.3).sum();
    a.ex(&mut c, "cell.set", json!({"cell": "E2", "input": "=C2*D2"}));
    a.ex(&mut c, "home.editing.fill_down", json!({"range": "E2:E9"}));
    let v = a.cell("E9", "input");
    c.check("fill down shifts relative refs", v == "=C9*D9", v);
    a.ex(
        &mut c,
        "cell.set",
        json!({"cell": "E10", "input": "=SUM(E2:E9)"}),
    );
    let v = a.cell("E10", "value");
    c.check("SUM of a formula column", close(&v, total), v);
    a.ex(
        &mut c,
        "cell.set",
        json!({"cell": "G1", "input": "=SUMIF(A2:A9,\"East\",E2:E9)"}),
    );
    let v = a.cell("G1", "value");
    c.check("SUMIF", close(&v, 15.0 + 21.0 + 11.25), v);
    a.ex(
        &mut c,
        "cell.set",
        json!({"cell": "G2", "input": "=XLOOKUP(\"Plums\",B2:B9,C2:C9)"}),
    );
    let v = a.cell("G2", "value");
    c.check("XLOOKUP", close(&v, 7.0), v);
    a.ex(
        &mut c,
        "cell.set",
        json!({"cell": "G3", "input": "=TEXT(E10,\"#,##0.00\")"}),
    );
    let v = a.cell("G3", "value");
    c.check("TEXT with a number format", v == "123.75", v);
    a.ex(
        &mut c,
        "home.number.format",
        json!({"range": "D2:E10", "code": "#,##0.00"}),
    );
    let v = (a.cell("D3", "display"), a.cell("C2", "display"));
    c.check("number format display", v.0 == "2.25" && v.1 == "10", v);

    // Dates edit as dates
    a.ex(
        &mut c,
        "cell.set",
        json!({"cell": "L1", "input": "2026-01-31"}),
    );
    let v = a.cell("L1", "input");
    c.check("a date edits as a date, not a serial", v == "1/31/2026", v);
    a.ex(
        &mut c,
        "home.editing.fill_series",
        json!({"range": "L1:L4", "type": "date", "date_unit": "month", "step": 1}),
    );
    let v = a.cell("L2", "input");
    c.check("fill series by month keeps month ends", v == "2/28/2026", v);

    // Clipboard and undo
    a.ex(&mut c, "home.clipboard.copy", json!({"range": "E2"}));
    a.ex(&mut c, "home.clipboard.paste", json!({"cell": "F5"}));
    let v = a.cell("F5", "input");
    c.check("paste shifts relative refs", v == "=D5*E5", v);
    a.ex(&mut c, "edit.undo", json!({}));
    let v = a.cell("F5", "input");
    c.check("undo paste", v == "" || v.is_null(), v);
    a.ex(&mut c, "cell.set", json!({"cell": "H1", "input": "=C2+1"}));
    a.ex(&mut c, "home.clipboard.cut", json!({"range": "C2"}));
    a.ex(&mut c, "home.clipboard.paste", json!({"cell": "J2"}));
    let v = (a.cell("J2", "value"), a.cell("C2", "value"));
    c.check("cut moves the value", close(&v.0, 10.0) && v.1.is_null(), v);
    let v = (a.cell("H1", "input"), a.cell("E2", "input"));
    c.check(
        "cut rewrites dependents",
        v.0 == "=J2+1" && v.1 == "=J2*D2",
        v,
    );
    a.ex(&mut c, "edit.undo", json!({}));
    let v = (a.cell("C2", "value"), a.cell("H1", "input"));
    c.check("undo cut restores", close(&v.0, 10.0) && v.1 == "=C2+1", v);
    a.ex(&mut c, "edit.redo", json!({}));
    let v = a.cell("J2", "value");
    c.check("redo cut", close(&v, 10.0), v);
    a.ex(&mut c, "edit.undo", json!({}));

    // Sort, filter, find, replace, insert rows
    a.ex(
        &mut c,
        "data.sort",
        json!({"range": "A1:E9", "keys": [{"column": "C", "order": "desc"}], "header": true}),
    );
    let qty: Vec<f64> = (2..=9)
        .map(|r| num(&a.cell(&format!("C{r}"), "value")))
        .collect();
    c.check(
        "sort descending",
        qty.windows(2).all(|w| w[0] >= w[1]),
        &qty,
    );
    let v = a.cell("E2", "value");
    c.check("formulas follow sorted rows", close(&v, 18.0), v);
    a.ex(&mut c, "edit.undo", json!({}));
    let v = a.cell("C2", "value");
    c.check("undo sort", close(&v, 10.0), v);
    let v = a.ex(
        &mut c,
        "data.filter",
        json!({"range": "A1:E9", "on": true, "column": "A", "values": ["East"]}),
    );
    c.check("filter applies", !v.is_null(), &v);
    a.ex(&mut c, "data.filter_clear", json!({}));
    let v = a.ex(
        &mut c,
        "edit.find_all",
        json!({"what": "Apples", "whole": true}),
    );
    c.check(
        "find all, whole cell",
        v.as_array().map_or(0, Vec::len) == 3,
        &v,
    );
    let v = a.ex(
        &mut c,
        "home.editing.replace",
        json!({"what": "South", "with": "Southern", "whole": true, "within": "workbook", "all": true}),
    );
    let a9 = a.cell("A9", "value");
    c.check("replace all", v == json!(1) && a9 == "Southern", (v, a9));
    a.ex(&mut c, "home.cells.insert_rows", json!({"range": "A5"}));
    let v = a.cell("E11", "input");
    c.check("insert row grows a SUM range", v == "=SUM(E2:E10)", v);
    a.ex(&mut c, "edit.undo", json!({}));
    let v = a.cell("E10", "input");
    c.check("undo insert row", v == "=SUM(E2:E9)", v);

    // Chart, objects, checkboxes
    a.ex(
        &mut c,
        "insert.chart.column",
        json!({"series": [{"values": "E2:E9", "name": "E1", "categories": "B2:B9"}], "title": "Totals"}),
    );
    a.ex(&mut c, "insert.picture", json!({"path": pic, "cell": "N2"}));
    a.ex(
        &mut c,
        "insert.shapes",
        json!({"shape": "rightArrow", "cell": "N8", "width": 100, "height": 40, "text": "Go"}),
    );
    a.ex(&mut c, "insert.checkbox", json!({"range": "F2:F4"}));
    a.ex(&mut c, "cell.toggle_checkbox", json!({"range": "F3"}));
    let v = a.cell("F3", "value");
    c.check("checkbox toggles", v == json!(true), v);
    let v = a.ex(&mut c, "object.list", json!({}));
    c.check("objects listed", object_count(&v) == 2, &v);

    // Pivot and slicer
    let pv = a.ex(
        &mut c,
        "insert.pivot_table",
        json!({"source": "Sheet1!A1:E9", "name": "PT1"}),
    );
    let pv_sheet = pv["sheet"].as_str().unwrap_or("Sheet2").to_string();
    a.ex(
        &mut c,
        "pivot.field",
        json!({"pivot": "PT1", "field": "Region", "area": "rows"}),
    );
    a.ex(
        &mut c,
        "pivot.field",
        json!({"pivot": "PT1", "field": "Total", "area": "values"}),
    );
    a.ex(
        &mut c,
        "insert.slicer",
        json!({"pivot": "PT1", "field": "Region", "anchor": "E2:G12"}),
    );
    let flat = values(&a.get("A1:B8"));
    c.check(
        "pivot shows the grand total",
        flat.iter().any(|v| close(v, total)),
        &flat,
    );
    c.check(
        "pivot lists regions",
        ["East", "West", "North"]
            .iter()
            .all(|r| flat.iter().any(|v| v == *r)),
        &flat,
    );

    // Save, PDF
    let xlsx = wd.join("smoke.xlsx");
    let pdf = wd.join("smoke.pdf");
    a.ex(&mut c, "sheet.activate", json!({"sheet": "Sheet1"}));
    let before = a.get("A1:L10");
    a.ex(&mut c, "file.save_as", json!({"path": xlsx}));
    let size = std::fs::metadata(&xlsx).map_or(0, |m| m.len());
    c.check("xlsx written", size > 2000, size);
    a.ex(
        &mut c,
        "file.export.pdf",
        json!({"path": pdf, "sheets": "all"}),
    );
    a.quit(&mut c, "first session exits cleanly");

    // Reopen in a fresh process
    let mut b = App::launch(bin, &log)?;
    b.ex(&mut c, "file.open", json!({"path": xlsx}));
    b.ex(&mut c, "sheet.activate", json!({"sheet": "Sheet1"}));
    let after = b.get("A1:L10");
    let same = |x: &Json, y: &Json| x["input"] == y["input"] && x["value"] == y["value"];
    let diffs: Vec<(Json, Json)> = before
        .as_array()
        .into_iter()
        .flatten()
        .zip(after.as_array().into_iter().flatten())
        .flat_map(|(rx, ry)| {
            rx.as_array()
                .into_iter()
                .flatten()
                .zip(ry.as_array().into_iter().flatten())
                .filter(|(x, y)| !same(x, y))
                .map(|(x, y)| (x.clone(), y.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    c.check(
        "reopened cells match (inputs and values)",
        before.is_array() && after.is_array() && diffs.is_empty(),
        diffs.iter().take(5).collect::<Vec<_>>(),
    );
    let v = b.ex(&mut c, "object.list", json!({}));
    c.check("objects survive the round trip", object_count(&v) == 2, &v);
    let v = b.cell("F3", "value");
    c.check("checkbox state survives", v == json!(true), v);
    b.ex(&mut c, "sheet.activate", json!({"sheet": pv_sheet}));
    let flat = values(&b.get("A1:B8"));
    c.check(
        "pivot survives the round trip",
        flat.iter().any(|v| close(v, total)),
        &flat,
    );
    b.quit(&mut c, "second session exits cleanly");

    // PDF structure and the app's own output
    let data = std::fs::read(&pdf).unwrap_or_default();
    let tail = String::from_utf8_lossy(&data[data.len().saturating_sub(64)..]).to_string();
    c.check(
        "pdf header and trailer",
        data.starts_with(b"%PDF-") && tail.trim_end().ends_with("%%EOF"),
        data.len(),
    );
    let has = |pat: &[u8]| data.windows(pat.len()).any(|w| w == pat);
    c.check(
        "pdf embeds the picture",
        has(b"/Subtype /Image") || has(b"/Subtype/Image"),
        "",
    );
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let bad: Vec<&str> = text
        .lines()
        .filter(|l| {
            let l = l.to_lowercase();
            l.contains("panic") || l.contains("error") || l.contains("fatal")
        })
        .take(5)
        .collect();
    c.check(
        "no panics or errors in the app's output",
        bad.is_empty(),
        &bad,
    );

    println!(
        "smoke: {}/{} checks passed (outputs in {})",
        c.passed,
        c.passed + c.failed.len(),
        wd.display()
    );
    if !c.failed.is_empty() {
        bail!("smoke test failed: {}", c.failed.join(", "));
    }
    Ok(())
}
