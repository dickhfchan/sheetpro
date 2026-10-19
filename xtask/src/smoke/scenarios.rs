//! Scenario areas for `cargo xtask smoke`: each launches the built app, drives it the way a
//! user would over the control port, and checks values, files and the rendered window.

use super::{Ctx, close, is_error, num, object_count, values};
use anyhow::{Context, Result};
use serde_json::{Value as Json, json};

/// A small sales table in A1:D9 (Region, Product, Qty, Price) plus Total = Qty*Price in E.
const ROWS: [(&str, &str, f64, f64); 8] = [
    ("East", "Apples", 10.0, 1.5),
    ("West", "Pears", 4.0, 2.25),
    ("East", "Plums", 7.0, 3.0),
    ("North", "Apples", 12.0, 1.5),
    ("West", "Apples", 3.0, 1.5),
    ("North", "Pears", 8.0, 2.25),
    ("East", "Pears", 5.0, 2.25),
    ("South", "Plums", 9.0, 3.0),
];

fn total() -> f64 {
    ROWS.iter().map(|r| r.2 * r.3).sum()
}

fn sales_table(a: &mut super::App, c: &mut Ctx) {
    let header = ["Region", "Product", "Qty", "Price", "Total"];
    let mut grid = vec![header.iter().map(|h| json!(h)).collect::<Vec<_>>()];
    for (r, p, q, pr) in ROWS {
        grid.push(vec![json!(r), json!(p), json!(q), json!(pr), Json::Null]);
    }
    for (i, row) in grid.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            if v.is_null() {
                continue;
            }
            let col = ["A", "B", "C", "D", "E"][j];
            let text = v.as_str().map_or_else(|| v.to_string(), str::to_string);
            a.set(c, &format!("{col}{}", i + 1), &text);
        }
    }
    a.set(c, "E2", "=C2*D2");
    a.ex(c, "home.editing.fill_down", json!({"range": "E2:E9"}));
}

fn png_size(path: &std::path::Path) -> Option<(u32, u32)> {
    let b = std::fs::read(path).ok()?;
    let w = u32::from_be_bytes(b.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(b.get(20..24)?.try_into().ok()?);
    b.starts_with(b"\x89PNG").then_some((w, h))
}

// ---------------------------------------------------------------------------------------

pub fn launch(c: &mut Ctx) -> Result<()> {
    let out = std::process::Command::new(&c.bin)
        .arg("--version")
        .output()?;
    let v = String::from_utf8_lossy(&out.stdout).to_string();
    c.check(
        "--version prints the version",
        out.status.success() && v.contains(env!("CARGO_PKG_VERSION")),
        &v,
    );
    let t = std::time::Instant::now();
    let mut a = c.launch()?;
    let secs = t.elapsed().as_secs_f64();
    c.check("starts within 10 s", secs < 10.0, secs);
    let st = a.state();
    c.check(
        "opens a clean Sheet1",
        st["sheets"] == json!(["Sheet1"]) && st["dirty"] == json!(false) && st["active"] == "A1",
        &st,
    );
    let n = a.raw(json!({"op": "commands"}))?["result"]
        .as_array()
        .map_or(0, Vec::len);
    c.check("all commands registered (>= 304)", n >= 304, n);
    let f = a.raw(json!({"op": "functions"}))?["result"]
        .as_array()
        .map_or(0, Vec::len);
    c.check("all functions registered (>= 502)", f >= 502, f);
    a.quit(c, "exits cleanly");
    Ok(())
}

pub fn formulas(c: &mut Ctx) -> Result<()> {
    let mut a = c.launch()?;
    // Inputs some formulas read.
    for (cell, v) in [
        ("H1", "Apples"),
        ("H2", "Pears"),
        ("H3", "Plums"),
        ("I1", "10"),
        ("I2", "20"),
        ("I3", "30"),
    ] {
        a.set(c, cell, v);
    }
    let cases: &[(&str, Json)] = &[
        // math
        ("=SUM(1,2,3)", json!(6.0)),
        ("=ROUND(2.675,2)", json!(2.68)),
        ("=MOD(-3,2)", json!(1.0)),
        ("=POWER(2,10)", json!(1024.0)),
        ("=SUMPRODUCT({1,2},{3,4})", json!(11.0)),
        ("=INT(-1.5)", json!(-2.0)),
        // text
        ("=CONCAT(\"a\",\"b\",1)", json!("ab1")),
        ("=LEFT(\"hello\",2)&UPPER(\"x\")", json!("heX")),
        ("=TEXTJOIN(\"-\",TRUE,\"a\",\"\",\"b\")", json!("a-b")),
        ("=SUBSTITUTE(\"aaa\",\"a\",\"b\",2)", json!("aba")),
        ("=TEXT(1234.5,\"$#,##0.00\")", json!("$1,234.50")),
        ("=TRIM(\"  a   b  \")", json!("a b")),
        // logical
        ("=IF(1>2,\"y\",\"n\")", json!("n")),
        ("=AND(TRUE,FALSE)", json!(false)),
        ("=IFS(FALSE,1,TRUE,2)", json!(2.0)),
        ("=SWITCH(2,1,\"a\",2,\"b\")", json!("b")),
        // lookup and reference
        ("=VLOOKUP(\"Pears\",H1:I3,2,FALSE)", json!(20.0)),
        ("=INDEX(I1:I3,MATCH(\"Plums\",H1:H3,0))", json!(30.0)),
        ("=XLOOKUP(\"Kiwi\",H1:H3,I1:I3,\"none\")", json!("none")),
        ("=ROWS(FILTER(H1:H3,I1:I3>15))", json!(2.0)),
        ("=INDEX(SORT(I1:I3,1,-1),1)", json!(30.0)),
        ("=COUNTA(UNIQUE({1;1;2}))", json!(2.0)),
        // date and time
        ("=DATE(2024,2,29)", json!(45351.0)),
        ("=EDATE(DATE(2024,1,31),1)", json!(45351.0)),
        ("=NETWORKDAYS(DATE(2026,1,1),DATE(2026,1,31))", json!(22.0)),
        ("=WEEKDAY(DATE(2026,10,19))", json!(2.0)),
        ("=TEXT(DATE(2026,1,31),\"yyyy-mm-dd\")", json!("2026-01-31")),
        ("=HOUR(TIME(13,30,0))", json!(13.0)),
        // statistics
        ("=AVERAGE(1,2,3,4)", json!(2.5)),
        ("=MEDIAN(1,3,2)", json!(2.0)),
        ("=STDEV.S(2,4,4,4,5,5,7,9)", json!(2.138_089_935_299_395)),
        ("=COUNTIF(I1:I3,\">15\")", json!(2.0)),
        ("=MAXIFS(I1:I3,H1:H3,\"<>Plums\")", json!(20.0)),
        // financial
        ("=PMT(0.05/12,360,200000)", json!(-1_073.643_246_024_28)),
        ("=NPV(0.1,100,100)", json!(173.553_719_008_264_5)),
        // information
        ("=ISNUMBER(1)", json!(true)),
        ("=ISBLANK(Z99)", json!(true)),
        ("=TYPE(\"a\")", json!(2.0)),
        // LET / LAMBDA / MAP
        ("=LET(x,2,LAMBDA(y,x*y)(5))", json!(10.0)),
        ("=SUM(MAP({1,2,3},LAMBDA(v,v*2)))", json!(12.0)),
        // errors
        ("=1/0", json!({"error": "#DIV/0!"})),
        ("=VLOOKUP(\"Kiwi\",H1:I3,2,FALSE)", json!({"error": "#N/A"})),
        ("=NOSUCHFUNCTION(1)", json!({"error": "#NAME?"})),
        ("=\"a\"+1", json!({"error": "#VALUE!"})),
        ("=IFERROR(1/0,\"caught\")", json!("caught")),
    ];
    for (i, (f, want)) in cases.iter().enumerate() {
        let cell = format!("A{}", i + 1);
        a.set(c, &cell, f);
        let got = a.val(&cell);
        let ok = match want {
            Json::Number(n) => close(&got, n.as_f64().unwrap_or(f64::NAN)),
            other => &got == other,
        };
        c.check(f, ok, &got);
    }

    // Known differences from the target.
    a.set(c, "K1", "=DATE(2026,2,28)");
    let d = a.cell("K1", "display");
    c.known(
        "a DATE formula displays as a date",
        d == "2/28/2026",
        d,
        "the target formats a cell whose formula returns a date",
    );
    a.set(c, "K2", "=LEN(\"héllo 😀\")");
    let v = a.val("K2");
    c.known(
        "LEN counts an emoji as 2",
        close(&v, 8.0),
        v,
        "the target counts UTF-16 units in all text functions",
    );

    // Spill, #SPILL! when blocked, recovery when cleared.
    a.set(c, "M1", "=SEQUENCE(3)");
    let v = values(&a.get("M1:M3"));
    c.check(
        "SEQUENCE spills",
        v == vec![json!(1.0), json!(2.0), json!(3.0)],
        &v,
    );
    a.set(c, "M2", "x");
    let v = a.val("M1");
    c.check("a blocked spill is #SPILL!", is_error(&v, "#SPILL!"), &v);
    a.ex(c, "home.editing.clear_contents", json!({"range": "M2"}));
    let v = a.val("M2");
    c.check("clearing the blocker re-spills", close(&v, 2.0), &v);

    // A long dependency chain recalculates on an edit.
    a.set(c, "N1", "1");
    a.set(c, "N2", "=N1+1");
    a.ex(c, "home.editing.fill_down", json!({"range": "N2:N500"}));
    a.set(c, "N1", "1000");
    let v = a.val("N500");
    c.check("a 500-cell chain recalculates", close(&v, 1499.0), &v);

    // Circular references are reported, not hung on.
    a.set(c, "O1", "=O2+1");
    a.set(c, "O2", "=O1+1");
    let st = a.state();
    c.check(
        "a circular reference is reported and the app answers",
        num(&st["calc"]["cycles"]) >= 1.0,
        &st["calc"],
    );
    a.ex(c, "home.editing.clear_contents", json!({"range": "O1:O2"}));

    // Manual calculation mode holds results until Calculate Now.
    a.ex(c, "formulas.calc_manual", json!({}));
    a.set(c, "P1", "1");
    a.set(c, "P2", "=P1*10");
    a.set(c, "P1", "2");
    let held = a.val("P2");
    a.ex(c, "formulas.calculate_now", json!({}));
    let now = a.val("P2");
    c.check(
        "manual calculation waits for Calculate Now",
        !close(&held, 20.0) && close(&now, 20.0),
        (&held, &now),
    );
    a.ex(c, "formulas.calc_automatic", json!({}));
    a.quit(c, "exits cleanly");
    Ok(())
}

/// One realistic session end to end, then a reopen in a fresh process.
pub fn workflow(c: &mut Ctx) -> Result<()> {
    let pic = c.path("pic.png");
    let px: Vec<u8> = (0..120u32 * 60)
        .flat_map(|i| [(i % 120 * 2) as u8, (i / 120 * 4) as u8, 180, 255])
        .collect();
    std::fs::write(
        &pic,
        sp_render::objects::encode_png(120, 60, px).context("encoding the test picture")?,
    )?;
    let mut a = c.launch()?;
    sales_table(&mut a, c);
    let total = total();
    let v = a.cell("E9", "input");
    c.check("fill down shifts relative refs", v == "=C9*D9", v);
    a.set(c, "E10", "=SUM(E2:E9)");
    let v = a.val("E10");
    c.check("SUM of a formula column", close(&v, total), v);
    a.ex(
        c,
        "home.number.format",
        json!({"range": "D2:E10", "code": "#,##0.00"}),
    );
    let v = (a.cell("D3", "display"), a.cell("C2", "display"));
    c.check("number format display", v.0 == "2.25" && v.1 == "10", v);
    a.set(c, "L1", "2026-01-31");
    let v = a.cell("L1", "input");
    c.check("a date edits as a date, not a serial", v == "1/31/2026", v);
    a.ex(
        c,
        "home.editing.fill_series",
        json!({"range": "L1:L4", "type": "date", "date_unit": "month", "step": 1}),
    );
    let v = a.cell("L2", "input");
    c.check("fill series by month keeps month ends", v == "2/28/2026", v);

    a.ex(
        c,
        "data.sort",
        json!({"range": "A1:E9", "keys": [{"column": "C", "order": "desc"}], "header": true}),
    );
    let qty: Vec<f64> = (2..=9).map(|r| num(&a.val(&format!("C{r}")))).collect();
    c.check(
        "sort descending",
        qty.windows(2).all(|w| w[0] >= w[1]),
        &qty,
    );
    let v = a.val("E2");
    c.check("formulas follow sorted rows", close(&v, 18.0), v);
    a.ex(c, "edit.undo", json!({}));
    let v = a.val("C2");
    c.check("undo sort", close(&v, 10.0), v);
    let v = a.ex(
        c,
        "data.filter",
        json!({"range": "A1:E9", "on": true, "column": "A", "values": ["East"]}),
    );
    c.check("filter applies", !v.is_null(), &v);
    a.ex(c, "data.filter_clear", json!({}));

    a.ex(
        c,
        "insert.chart.column",
        json!({"series": [{"values": "E2:E9", "name": "E1", "categories": "B2:B9"}], "title": "Totals"}),
    );
    a.ex(c, "insert.picture", json!({"path": pic, "cell": "N2"}));
    a.ex(
        c,
        "insert.shapes",
        json!({"shape": "rightArrow", "cell": "N8", "width": 100, "height": 40, "text": "Go"}),
    );
    a.ex(c, "insert.checkbox", json!({"range": "F2:F4"}));
    a.ex(c, "cell.toggle_checkbox", json!({"range": "F3"}));
    let v = a.val("F3");
    c.check("checkbox toggles", v == json!(true), v);
    let v = a.ex(c, "object.list", json!({}));
    c.check("objects listed", object_count(&v) == 2, &v);

    let pv = a.ex(
        c,
        "insert.pivot_table",
        json!({"source": "Sheet1!A1:E9", "name": "PT1"}),
    );
    let pv_sheet = pv["sheet"].as_str().unwrap_or("Sheet2").to_string();
    a.ex(
        c,
        "pivot.field",
        json!({"pivot": "PT1", "field": "Region", "area": "rows"}),
    );
    a.ex(
        c,
        "pivot.field",
        json!({"pivot": "PT1", "field": "Total", "area": "values"}),
    );
    a.ex(
        c,
        "insert.slicer",
        json!({"pivot": "PT1", "field": "Region", "anchor": "E2:G12"}),
    );
    let flat = values(&a.get("A1:B8"));
    c.check(
        "pivot shows the grand total",
        flat.iter().any(|v| close(v, total)),
        &flat,
    );
    a.ex(
        c,
        "insert.slicer",
        json!({"name": "Slicer_Region", "select": ["East"]}),
    );
    let flat = values(&a.get("A1:B8"));
    c.check(
        "slicer selection filters the pivot",
        flat.iter().any(|v| close(v, 47.25)) && !flat.iter().any(|v| v == "West"),
        &flat,
    );
    a.ex(
        c,
        "insert.slicer",
        json!({"name": "Slicer_Region", "clear": true}),
    );

    let xlsx = c.path("workflow.xlsx");
    let pdf = c.path("workflow.pdf");
    a.ex(c, "sheet.activate", json!({"sheet": "Sheet1"}));
    let before = a.get("A1:L10");
    a.ex(c, "file.save_as", json!({"path": xlsx}));
    let st = a.state();
    c.check(
        "saving clears the dirty flag",
        st["dirty"] == json!(false),
        &st["dirty"],
    );
    a.ex(c, "file.export.pdf", json!({"path": pdf, "sheets": "all"}));
    a.quit(c, "first session exits cleanly");

    let mut b = c.launch()?;
    b.ex(c, "file.open", json!({"path": xlsx}));
    b.ex(c, "sheet.activate", json!({"sheet": "Sheet1"}));
    let after = b.get("A1:L10");
    let diffs = grid_diffs(&before, &after);
    c.check(
        "reopened cells match (inputs and values)",
        before.is_array() && diffs.is_empty(),
        &diffs,
    );
    let v = b.ex(c, "object.list", json!({}));
    c.check("objects survive the round trip", object_count(&v) == 2, &v);
    let v = b.val("F3");
    c.check("checkbox state survives", v == json!(true), v);
    b.ex(c, "sheet.activate", json!({"sheet": pv_sheet}));
    let flat = values(&b.get("A1:B8"));
    c.check(
        "pivot survives the round trip",
        flat.iter().any(|v| close(v, total)),
        &flat,
    );
    b.quit(c, "second session exits cleanly");

    let data = std::fs::read(&pdf).unwrap_or_default();
    check_pdf(c, "pdf", &data);
    let has = |pat: &[u8]| data.windows(pat.len()).any(|w| w == pat);
    c.check(
        "pdf embeds the picture",
        has(b"/Subtype /Image") || has(b"/Subtype/Image"),
        "",
    );
    Ok(())
}

/// Cells whose input or value differ between two `get` grids of the same range (values of
/// volatile formulas are not compared).
pub fn grid_diffs(before: &Json, after: &Json) -> Vec<(Json, Json)> {
    let rows = |g: &Json| g.as_array().cloned().unwrap_or_default();
    let (rb, ra) = (rows(before), rows(after));
    if rb.len() != ra.len() {
        return vec![(json!(rb.len()), json!(ra.len()))];
    }
    rb.iter()
        .zip(&ra)
        .flat_map(|(x, y)| {
            let (x, y) = (rows(x), rows(y));
            x.into_iter()
                .zip(y)
                .filter(|(p, q)| {
                    p["input"] != q["input"] || (p["value"] != q["value"] && !volatile(&p["input"]))
                })
                .collect::<Vec<_>>()
        })
        .take(5)
        .collect()
}

/// A formula whose value changes on every recalculation (time, random numbers).
fn volatile(input: &Json) -> bool {
    let f = input.as_str().unwrap_or("").to_ascii_uppercase();
    f.starts_with('=')
        && ["NOW(", "TODAY(", "RAND(", "RANDBETWEEN(", "RANDARRAY("]
            .iter()
            .any(|v| f.contains(v))
}

pub fn check_pdf(c: &mut Ctx, what: &str, data: &[u8]) {
    let tail = String::from_utf8_lossy(&data[data.len().saturating_sub(64)..]).to_string();
    c.check(
        &format!("{what} is a complete PDF"),
        data.starts_with(b"%PDF-") && tail.trim_end().ends_with("%%EOF"),
        data.len(),
    );
}

pub fn editing(c: &mut Ctx) -> Result<()> {
    let mut a = c.launch()?;
    sales_table(&mut a, c);
    a.set(c, "G1", "=SUM(E2:E9)");
    let total = total();

    // Copy/paste variants.
    a.ex(c, "home.clipboard.copy", json!({"range": "E2"}));
    a.ex(c, "home.clipboard.paste", json!({"cell": "F5"}));
    let v = a.cell("F5", "input");
    c.check("paste shifts relative refs", v == "=D5*E5", v);
    a.ex(c, "edit.undo", json!({}));
    a.ex(c, "home.clipboard.copy", json!({"range": "E2:E3"}));
    a.ex(c, "home.clipboard.paste_values", json!({"cell": "J1"}));
    let v = (a.cell("J1", "input"), a.val("J2"));
    c.check(
        "paste values drops formulas",
        v.0 == "15" && close(&v.1, 9.0),
        v,
    );
    a.ex(c, "home.clipboard.copy", json!({"range": "A1:C1"}));
    a.ex(c, "home.clipboard.paste_transpose", json!({"cell": "K1"}));
    let v = values(&a.get("K1:K3"));
    c.check(
        "paste transpose",
        v == vec![json!("Region"), json!("Product"), json!("Qty")],
        &v,
    );
    a.ex(c, "home.clipboard.copy", json!({"range": "C2"}));
    a.ex(
        c,
        "home.clipboard.paste_special",
        json!({"range": "C3:C4", "mode": "values", "operation": "add"}),
    );
    let v = (a.val("C3"), a.val("C4"));
    c.check(
        "paste special add",
        close(&v.0, 14.0) && close(&v.1, 17.0),
        v,
    );
    a.ex(c, "edit.undo", json!({}));

    // Cut is a move: dependents follow, undo/redo restore.
    a.set(c, "H1", "=C2+1");
    a.ex(c, "home.clipboard.cut", json!({"range": "C2"}));
    a.ex(c, "home.clipboard.paste", json!({"cell": "L2"}));
    let v = (a.cell("H1", "input"), a.cell("E2", "input"), a.val("C2"));
    c.check(
        "cut moves and rewrites dependents",
        v.0 == "=L2+1" && v.1 == "=L2*D2" && v.2.is_null(),
        v,
    );
    a.ex(c, "edit.undo", json!({}));
    let v = (a.val("C2"), a.cell("H1", "input"));
    c.check("undo cut restores", close(&v.0, 10.0) && v.1 == "=C2+1", v);
    a.ex(c, "edit.redo", json!({}));
    let v = a.val("L2");
    c.check("redo cut", close(&v, 10.0), v);
    a.ex(c, "edit.undo", json!({}));

    // Rows and columns: references follow, deleted ones become #REF!.
    a.ex(c, "home.cells.insert_rows", json!({"range": "A5"}));
    let v = (a.cell("G1", "input"), a.val("G1"));
    c.check(
        "insert row grows a SUM range",
        v.0 == "=SUM(E2:E10)" && close(&v.1, total),
        v,
    );
    a.ex(c, "edit.undo", json!({}));
    a.ex(c, "home.cells.insert_columns", json!({"range": "B1"}));
    let v = a.cell("H1", "input");
    c.check("insert column shifts refs", v == "=SUM(F2:F9)", v);
    a.ex(c, "edit.undo", json!({}));
    a.set(c, "H2", "=E9");
    a.ex(c, "home.cells.delete_rows", json!({"range": "A9"}));
    let v = (a.cell("H2", "input"), a.val("H2"));
    c.check(
        "deleting a referenced row gives #REF!",
        v.0.as_str().is_some_and(|s| s.contains("#REF!")) && is_error(&v.1, "#REF!"),
        v,
    );
    a.ex(c, "edit.undo", json!({}));
    let v = a.cell("H2", "input");
    c.check("undo delete row restores the ref", v == "=E9", v);

    // Fill series.
    a.set(c, "N1", "5");
    a.ex(
        c,
        "home.editing.fill_series",
        json!({"range": "N1:N6", "type": "linear", "step": 5}),
    );
    let v = values(&a.get("N1:N6"));
    c.check(
        "linear fill series",
        v == vec![
            json!(5.0),
            json!(10.0),
            json!(15.0),
            json!(20.0),
            json!(25.0),
            json!(30.0),
        ],
        &v,
    );
    a.set(c, "O1", "Mon");
    a.ex(c, "home.editing.fill_down", json!({"range": "O1:O3"}));
    let v = values(&a.get("O1:O3"));
    c.check(
        "fill down copies text",
        v == vec![json!("Mon"), json!("Mon"), json!("Mon")],
        &v,
    );
    a.set(c, "P1", "Item 1");
    a.ex(c, "cell.autofill", json!({"source": "P1", "dest": "P1:P3"}));
    let v = values(&a.get("P1:P3"));
    c.check(
        "autofill numbers text series",
        v == vec![json!("Item 1"), json!("Item 2"), json!("Item 3")],
        &v,
    );
    a.set(c, "Q1", "Mon");
    a.ex(c, "cell.autofill", json!({"source": "Q1", "dest": "Q1:Q3"}));
    let v = values(&a.get("Q1:Q3"));
    c.check(
        "autofill weekday names",
        v == vec![json!("Mon"), json!("Tue"), json!("Wed")],
        &v,
    );

    // Find / replace, merge, clear.
    let hits = a.ex(c, "edit.find_all", json!({"what": "Apples", "whole": true}));
    c.check(
        "find all, whole cell",
        hits.as_array().map_or(0, Vec::len) == 3,
        &hits,
    );
    let hits = a.ex(c, "edit.find_all", json!({"what": "?ears"}));
    c.check(
        "find with a ? wildcard",
        hits.as_array().map_or(0, Vec::len) == 3,
        &hits,
    );
    let n = a.ex(
        c,
        "home.editing.replace",
        json!({"what": "South", "with": "Southern", "whole": true, "all": true}),
    );
    let v = a.val("A9");
    c.check("replace all", n == json!(1) && v == "Southern", (n, v));
    a.ex(c, "home.align.merge_center", json!({"range": "Q1:S1"}));
    a.ex(c, "edit.undo", json!({}));
    a.ex(c, "home.editing.clear_all", json!({"range": "N1:N6"}));
    let v = a.val("N3");
    c.check("clear all empties cells", v.is_null(), v);

    // A long undo history unwinds completely.
    for i in 1..=30 {
        a.set(c, "R1", &i.to_string());
    }
    for _ in 0..30 {
        a.ex(c, "edit.undo", json!({}));
    }
    let v = a.val("R1");
    c.check("30 undos unwind 30 edits", v.is_null(), v);
    a.quit(c, "exits cleanly");
    Ok(())
}

pub fn formatting(c: &mut Ctx) -> Result<()> {
    let mut a = c.launch()?;
    for (cell, v) in [
        ("A1", "1234.567"),
        ("A2", "-42"),
        ("A3", "0.256"),
        ("A4", "46053"),
        ("A5", "0.5625"),
    ] {
        a.set(c, cell, v);
    }
    let cases = [
        ("A1", "#,##0.00", "1,234.57"),
        ("A1", "$#,##0", "$1,235"),
        ("A2", "0;[Red](0)", "(42)"),
        ("A3", "0.0%", "25.6%"),
        ("A4", "d-mmm-yyyy", "31-Jan-2026"),
        ("A5", "h:mm AM/PM", "1:30 PM"),
        ("A1", "0.00E+00", "1.23E+03"),
        ("A1", "@", "1234.567"),
    ];
    for (cell, code, want) in cases {
        a.ex(
            c,
            "home.number.format",
            json!({"range": cell, "code": code}),
        );
        let d = a.cell(cell, "display");
        c.check(&format!("format {code}"), d == want, d);
    }
    a.ex(c, "home.number.percent", json!({"range": "A3"}));
    let v = (a.cell("A3", "display"), a.cell("A3", "input"));
    c.check(
        "percent button and percent editing",
        v.0 == "26%" && v.1 == "25.6%",
        v,
    );
    a.set(c, "B1", "12%");
    let v = (a.val("B1"), a.cell("B1", "display"));
    c.check(
        "typing 12% stores 0.12 shown as 12%",
        close(&v.0, 0.12) && v.1 == "12%",
        v,
    );
    a.set(c, "B2", "$1,500");
    let v = a.val("B2");
    c.check("typing $1,500 stores a number", close(&v, 1500.0), v);

    // Styles, CF, merge, sizes: commands succeed and survive save/reopen.
    for (id, p) in [
        ("home.font.bold", json!({"range": "A1:A2"})),
        ("home.font.italic", json!({"range": "A1"})),
        (
            "home.font.font_color",
            json!({"range": "A1", "color": "C00000"}),
        ),
        (
            "home.font.fill_color",
            json!({"range": "A2", "color": "FFFF00"}),
        ),
        (
            "home.font.borders",
            json!({"range": "A1:B5", "style": "thin"}),
        ),
        ("home.align.wrap_text", json!({"range": "C1"})),
        ("home.align.merge_center", json!({"range": "D1:F1"})),
        (
            "home.cells.column_width",
            json!({"range": "A1", "width": 18}),
        ),
        (
            "home.cells.row_height",
            json!({"range": "A1", "height": 30}),
        ),
        ("view.freeze_panes", json!({"cell": "B2"})),
        (
            "home.styles.cf.highlight_rules",
            json!({"range": "A1:A5", "rule": "greater_than", "value": 100}),
        ),
        ("home.styles.cf.data_bars", json!({"range": "A1:A5"})),
        ("home.styles.cf.color_scales", json!({"range": "B1:B5"})),
    ] {
        a.ex(c, id, p);
    }
    a.set(c, "D1", "Merged title");
    let rules = a.ex(
        c,
        "home.styles.cf.manage",
        json!({"action": "list", "scope": "sheet"}),
    );
    let n_rules = rules.as_array().map_or(0, Vec::len);
    c.check("conditional formats listed", n_rules == 3, &rules);
    let xlsx = c.path("formats.xlsx");
    let before = a.get("A1:F5");
    a.ex(c, "file.save_as", json!({"path": xlsx}));
    a.quit(c, "exits cleanly");

    let mut b = c.launch()?;
    b.ex(c, "file.open", json!({"path": xlsx}));
    let after = b.get("A1:F5");
    let disp = |g: &Json| -> Vec<Json> {
        g.as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_array)
            .flatten()
            .map(|c| c["display"].clone())
            .collect()
    };
    c.check(
        "number formats survive save and reopen",
        disp(&before) == disp(&after),
        (disp(&before), disp(&after)),
    );
    let rules = b.ex(
        c,
        "home.styles.cf.manage",
        json!({"action": "list", "scope": "sheet"}),
    );
    c.check(
        "conditional formats survive save and reopen",
        rules.as_array().map_or(0, Vec::len) == n_rules,
        &rules,
    );
    b.quit(c, "reopened session exits cleanly");
    Ok(())
}

pub fn data_tools(c: &mut Ctx) -> Result<()> {
    let mut a = c.launch()?;
    sales_table(&mut a, c);

    // Multi-key sort.
    a.ex(
        c,
        "data.sort",
        json!({"range": "A1:E9", "header": true,
               "keys": [{"column": "A", "order": "asc"}, {"column": "C", "order": "desc"}]}),
    );
    let v: Vec<Json> = (2..=4).map(|r| a.val(&format!("C{r}"))).collect();
    c.check(
        "two-key sort (East by Qty desc)",
        a.val("A2") == "East" && v == vec![json!(10.0), json!(7.0), json!(5.0)],
        &v,
    );
    a.ex(c, "edit.undo", json!({}));

    // Custom filter: rows hidden, SUBTOTAL sees only visible ones.
    a.set(c, "G1", "=SUBTOTAL(109,E2:E9)");
    a.ex(
        c,
        "data.filter",
        json!({"range": "A1:E9", "on": true, "column": "C", "custom": [{"op": ">=", "val": 8}]}),
    );
    let v = a.val("G1");
    c.check(
        "custom filter + SUBTOTAL(109)",
        close(&v, 15.0 + 18.0 + 18.0 + 27.0),
        v,
    );
    a.ex(c, "data.filter_clear", json!({}));
    let v = a.val("G1");
    c.check(
        "clearing the filter restores SUBTOTAL",
        close(&v, total()),
        v,
    );
    a.ex(c, "data.filter", json!({"range": "A1:E9", "on": false}));

    // Table with structured references and a total row.
    let t = a.ex(
        c,
        "insert.table",
        json!({"range": "A1:E9", "name": "Sales"}),
    );
    c.check("insert table", t["name"] == "Sales", &t);
    a.set(c, "H1", "=SUM(Sales[Total])");
    let v = a.val("H1");
    c.check("structured reference", close(&v, total()), v);
    a.ex(
        c,
        "table.design",
        json!({"name": "Sales", "total_row": true}),
    );

    // Validation: a list rule rejects other input.
    a.ex(
        c,
        "data.validation",
        json!({"range": "J1:J5", "type": "list", "source": "Yes,No"}),
    );
    let ok = a.try_ex("cell.set", json!({"cell": "J1", "input": "Yes"}));
    let bad = a.try_ex("cell.set", json!({"cell": "J2", "input": "Maybe"}));
    c.check(
        "validation accepts listed and rejects other input",
        ok.is_ok() && bad.is_err() && a.val("J2").is_null(),
        (&ok, &bad),
    );

    // Remove duplicates, text to columns, goal seek.
    for (cell, v) in [
        ("L1", "k"),
        ("L2", "a"),
        ("L3", "b"),
        ("L4", "a"),
        ("L5", "a"),
    ] {
        a.set(c, cell, v);
    }
    a.ex(
        c,
        "data.remove_duplicates",
        json!({"range": "L1:L5", "header": true}),
    );
    let v = values(&a.get("L1:L5"));
    c.check(
        "remove duplicates",
        v == vec![json!("k"), json!("a"), json!("b"), Json::Null, Json::Null],
        &v,
    );
    a.set(c, "N1", "red,green,blue");
    a.ex(
        c,
        "data.text_to_columns",
        json!({"range": "N1", "delimiter": ","}),
    );
    let v = values(&a.get("N1:P1"));
    c.check(
        "text to columns",
        v == vec![json!("red"), json!("green"), json!("blue")],
        &v,
    );
    a.set(c, "R1", "10");
    a.set(c, "R2", "=R1^2+R1");
    a.ex(
        c,
        "data.what_if.goal_seek",
        json!({"set_cell": "R2", "to_value": 156, "by_changing": "R1"}),
    );
    let v = a.val("R1");
    c.check("goal seek", (num(&v) - 12.0).abs() < 1e-3, v);
    a.quit(c, "exits cleanly");
    Ok(())
}

pub fn sheets_and_names(c: &mut Ctx) -> Result<()> {
    let mut a = c.launch()?;
    a.set(c, "A1", "5");
    a.ex(c, "home.cells.insert_sheet", json!({"name": "Data"}));
    a.ex(c, "sheet.activate", json!({"sheet": "Data"}));
    a.set(c, "A1", "=Sheet1!A1*2");
    a.ex(
        c,
        "home.cells.rename_sheet",
        json!({"sheet": "Sheet1", "name": "My Inputs"}),
    );
    let v = (a.cell("A1", "input"), a.val("A1"));
    c.check(
        "renaming a sheet rewrites references",
        v.0 == "='My Inputs'!A1*2" && close(&v.1, 10.0),
        v,
    );
    a.ex(
        c,
        "formulas.define_name",
        json!({"name": "Rate", "refers_to": "='My Inputs'!$A$1"}),
    );
    a.set(c, "B1", "=Rate*3");
    let v = a.val("B1");
    c.check("defined name", close(&v, 15.0), v);
    a.ex(c, "sheet.duplicate", json!({"sheet": "Data"}));
    let sheets = a.state()["sheets"].clone();
    c.check(
        "duplicate sheet",
        sheets.as_array().map_or(0, Vec::len) == 3,
        &sheets,
    );
    a.ex(
        c,
        "home.cells.move_copy_sheet",
        json!({"sheet": "Data", "to": 0}),
    );
    let sheets = a.state()["sheets"].clone();
    c.check("move sheet first", sheets[0] == "Data", &sheets);
    a.ex(c, "home.cells.hide_sheet", json!({"sheet": "My Inputs"}));
    let v = a.val("A1");
    c.check("formulas still read a hidden sheet", close(&v, 10.0), v);
    a.ex(c, "home.cells.unhide_sheet", json!({"sheet": "My Inputs"}));

    // Protection blocks edits until removed.
    a.ex(c, "sheet.activate", json!({"sheet": "Data"}));
    a.ex(
        c,
        "home.cells.protect_sheet",
        json!({"protect": true, "password": "pw"}),
    );
    let blocked = a.try_ex("cell.set", json!({"cell": "C1", "input": "x"}));
    c.check(
        "a protected sheet refuses edits",
        blocked.is_err(),
        &blocked,
    );
    let wrong = a.try_ex(
        "home.cells.protect_sheet",
        json!({"protect": false, "password": "nope"}),
    );
    c.check(
        "a wrong password does not unprotect",
        wrong.is_err(),
        &wrong,
    );
    a.ex(
        c,
        "home.cells.protect_sheet",
        json!({"protect": false, "password": "pw"}),
    );
    let ok = a.try_ex("cell.set", json!({"cell": "C1", "input": "x"}));
    c.check("unprotected sheet accepts edits", ok.is_ok(), &ok);

    // Deleting a sheet that others reference gives #REF!, and undo restores it.
    a.ex(c, "home.cells.delete_sheet", json!({"sheet": "My Inputs"}));
    let v = a.val("A1");
    c.check(
        "deleting a referenced sheet gives #REF!",
        is_error(&v, "#REF!"),
        v,
    );
    a.ex(c, "edit.undo", json!({}));
    let v = a.val("A1");
    c.check("undo delete sheet", close(&v, 10.0), v);

    // Comments, notes and links survive a round trip.
    a.ex(c, "insert.note", json!({"cell": "D1", "text": "a note"}));
    a.ex(
        c,
        "insert.comment",
        json!({"cell": "D2", "text": "a comment", "author": "QA"}),
    );
    a.ex(
        c,
        "insert.hyperlink",
        json!({"cell": "D3", "address": "https://example.com", "text": "site"}),
    );
    let xlsx = c.path("sheets.xlsx");
    a.ex(c, "file.save_as", json!({"path": xlsx}));
    a.quit(c, "exits cleanly");
    let mut b = c.launch()?;
    b.ex(c, "file.open", json!({"path": xlsx}));
    let st = b.state();
    c.check(
        "sheet order and names survive",
        st["sheets"]
            .as_array()
            .is_some_and(|s| s.len() == 3 && s[0] == "Data"),
        &st["sheets"],
    );
    b.ex(c, "sheet.activate", json!({"sheet": "Data"}));
    let v = (b.val("B1"), b.val("D3"));
    c.check(
        "names and links survive",
        close(&v.0, 15.0) && v.1 == "site",
        v,
    );
    let names = b.ex(c, "formulas.name_manager", json!({}));
    c.check(
        "defined name survives",
        names.to_string().contains("Rate"),
        &names,
    );
    b.quit(c, "reopened session exits cleanly");
    Ok(())
}

pub fn files(c: &mut Ctx) -> Result<()> {
    let mut a = c.launch()?;
    let unicode = "Ünïcödé 数据 😀";
    a.set(c, "A1", unicode);
    a.set(c, "A2", "3.5");
    a.set(c, "A3", "=A2*2");
    a.set(c, "A4", "with, comma \"quoted\"");
    a.set(c, "B1", "line1");
    a.ex(
        c,
        "home.cells.rename_sheet",
        json!({"sheet": "Sheet1", "name": "Données"}),
    );

    // xlsx to a path with spaces and non-ASCII characters.
    let dir = c.path("dir with spaces ü");
    std::fs::create_dir_all(&dir)?;
    let xlsx = dir.join("book é.xlsx");
    a.ex(c, "file.save_as", json!({"path": xlsx}));
    let csv = c.path("book.csv");
    let tsv = c.path("book.tsv");
    a.ex(c, "file.save_as", json!({"path": csv}));
    a.ex(c, "file.save_as", json!({"path": tsv}));
    a.ex(c, "file.save_copy", json!({"path": c.path("copy.xlsx")}));
    let st = a.state();
    c.check(
        "save copy leaves the open path alone",
        st["path"].as_str().is_some_and(|p| p.ends_with("book.tsv")),
        &st["path"],
    );

    // Opening bad files fails cleanly and leaves the app usable.
    let empty = c.path("empty.xlsx");
    std::fs::write(&empty, b"")?;
    let junk = c.path("junk.xlsx");
    std::fs::write(
        &junk,
        (0..4096u32)
            .map(|i| (i * 7919 % 251) as u8)
            .collect::<Vec<_>>(),
    )?;
    let trunc = c.path("truncated.xlsx");
    let good = std::fs::read(&xlsx).unwrap_or_default();
    std::fs::write(&trunc, &good[..good.len() / 2])?;
    for (what, p) in [
        ("an empty file", empty),
        ("random bytes", junk),
        ("a truncated xlsx", trunc),
        ("a missing file", c.path("missing.xlsx")),
    ] {
        let r = a.try_ex("file.open", json!({"path": p}));
        let alive = a.alive();
        c.check(
            &format!("opening {what} fails cleanly"),
            r.is_err() && alive,
            &r,
        );
    }
    let v = a.val("A1");
    c.check(
        "the open workbook is untouched by failed opens",
        v == unicode,
        v,
    );
    let ro = c.path("readonly");
    std::fs::create_dir_all(&ro)?;
    let orig = std::fs::metadata(&ro)?.permissions();
    let mut perm = orig.clone();
    perm.set_readonly(true);
    std::fs::set_permissions(&ro, perm)?;
    let r = a.try_ex("file.save_as", json!({"path": ro.join("x.xlsx")}));
    c.check(
        "saving into a read-only folder fails cleanly",
        r.is_err() && a.alive(),
        &r,
    );
    std::fs::set_permissions(&ro, orig)?;
    a.quit(c, "exits cleanly");

    // Reopen each format.
    let mut b = c.launch()?;
    b.ex(c, "file.open", json!({"path": xlsx}));
    let v = (
        b.state()["sheets"].clone(),
        b.val("A1"),
        b.cell("A3", "input"),
    );
    c.check(
        "xlsx with a unicode path, sheet name and text",
        v.0 == json!(["Données"]) && v.1 == unicode && v.2 == "=A2*2",
        v,
    );
    b.ex(c, "file.open", json!({"path": csv}));
    let v = (b.val("A1"), b.val("A3"), b.val("A4"));
    c.check(
        "csv round trip (values, quoting, unicode)",
        v.0 == unicode && close(&v.1, 7.0) && v.2 == "with, comma \"quoted\"",
        v,
    );
    b.ex(c, "file.open", json!({"path": tsv}));
    let v = (b.val("A1"), b.val("B1"));
    c.check("tsv round trip", v.0 == unicode && v.1 == "line1", v);
    b.ex(c, "file.new", json!({}));
    let st = b.state();
    c.check(
        "file.new gives an empty workbook",
        st["path"].is_null() && b.val("A1").is_null(),
        &st,
    );
    b.quit(c, "reopened session exits cleanly");

    // A workbook written by the library (not the app) opens in the app.
    let lib = c.path("library.xlsx");
    let mut wb = sp_model::Workbook::new();
    if let Some(sh) = wb.sheets.first_mut() {
        sh.set(
            sp_core::CellAddr::new(0, 0),
            sp_model::Cell::constant(sp_core::Value::Number(41.0)),
        );
        sh.set(
            sp_core::CellAddr::new(1, 0),
            sp_model::Cell::formula("A1+1"),
        );
    }
    sp_io::save(&wb, &lib).context("writing library.xlsx")?;
    let mut d = c.launch()?;
    d.ex(c, "file.open", json!({"path": lib}));
    let v = d.val("A2");
    c.check(
        "an externally written xlsx opens and recalculates",
        close(&v, 42.0),
        v,
    );
    d.quit(c, "third session exits cleanly");
    Ok(())
}

pub fn print(c: &mut Ctx) -> Result<()> {
    let mut a = c.launch()?;
    for r in 1..=200 {
        a.set(c, &format!("A{r}"), &format!("Row {r}"));
        a.set(c, &format!("B{r}"), &(r * 3).to_string());
    }
    let one = c.path("one.pdf");
    let r = a.ex(c, "file.export.pdf", json!({"path": one}));
    let pages = num(&r["pages"]);
    c.check("200 rows paginate onto several pages", pages >= 3.0, &r);
    check_pdf(
        c,
        "multi-page pdf",
        &std::fs::read(&one).unwrap_or_default(),
    );
    a.ex(c, "layout.orientation", json!({"orientation": "landscape"}));
    a.ex(c, "layout.print_area.set", json!({"range": "A1:B20"}));
    a.ex(
        c,
        "insert.header_footer",
        json!({"header": "&CQuarterly Report", "footer": "&RPage &P of &N"}),
    );
    let two = c.path("area.pdf");
    let r = a.ex(c, "file.export.pdf", json!({"path": two}));
    c.check("a print area prints one page", num(&r["pages"]) == 1.0, &r);
    let data = std::fs::read(&two).unwrap_or_default();
    check_pdf(c, "print-area pdf", &data);
    let mb = media_box(&data);
    c.check("landscape page size", mb.is_some_and(|(w, h)| w > h), mb);
    a.ex(c, "layout.page_break.insert", json!({"cell": "A10"}));
    a.ex(c, "layout.print_area.clear", json!({}));
    a.ex(c, "view.page_break_preview", json!({}));
    a.ex(c, "view.page_layout", json!({}));
    a.ex(c, "view.normal", json!({}));
    let xlsx = c.path("print.xlsx");
    a.ex(c, "file.save_as", json!({"path": xlsx}));
    a.ex(c, "home.cells.insert_sheet", json!({"name": "Empty"}));
    let three = c.path("all.pdf");
    let r = a.try_ex("file.export.pdf", json!({"path": three, "sheets": "all"}));
    c.check(
        "exporting all sheets with an empty one works",
        r.is_ok(),
        &r,
    );
    a.quit(c, "exits cleanly");
    Ok(())
}

/// Width and height of the first `/MediaBox [x0 y0 x1 y1]` in a PDF.
fn media_box(data: &[u8]) -> Option<(f64, f64)> {
    let text = String::from_utf8_lossy(data);
    let at = text.find("/MediaBox")?;
    let rest = &text[at..];
    let open = rest.find('[')?;
    let close = rest.find(']')?;
    let n: Vec<f64> = rest
        .get(open + 1..close)?
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    match n.as_slice() {
        [x0, y0, x1, y1] => Some((x1 - x0, y1 - y0)),
        _ => None,
    }
}

pub fn hostile(c: &mut Ctx) -> Result<()> {
    let mut a = c.launch()?;
    let long = "x".repeat(32_767);
    let nested = format!("={}1{}", "(".repeat(200), ")".repeat(200));
    let inputs: Vec<(&str, String)> = vec![
        ("the last cell", "XFD1048576".into()),
        ("a 32,767-character string", long.clone()),
        ("200 nested parentheses", nested),
        ("a lone =", "=".into()),
        ("an unclosed formula", "=SUM(A1:".into()),
        ("a reference past the grid", "=XFE1+A1048577".into()),
        ("a huge number", "1e308*10".into()),
        ("control characters", "a\u{0}b\u{7}c\td".into()),
        ("right-to-left text", "مرحبا بالعالم".into()),
        (
            "a 10,000-term formula",
            format!("={}", vec!["1"; 10_000].join("+")),
        ),
    ];
    for (what, input) in &inputs {
        let cell = if *what == "the last cell" {
            "XFD1048576"
        } else {
            "A1"
        };
        let text = if *what == "the last cell" {
            "edge"
        } else {
            input.as_str()
        };
        let r = a.try_ex("cell.set", json!({"cell": cell, "input": text}));
        let alive = a.alive();
        c.check(
            &format!("{what}: handled, app answers"),
            alive,
            r.map(|_| "ok"),
        );
    }
    let v = a.val("XFD1048576");
    c.check("the last cell stores a value", v == "edge", v);
    a.set(c, "B1", &long);
    let v = a.val("B1");
    c.check(
        "a 32,767-character string is kept whole",
        v.as_str().map_or(0, str::len) == 32_767,
        "",
    );
    for (id, p) in [
        ("select.range", json!({"range": "A1:XFD1048576"})),
        ("home.font.bold", json!({})),
        ("home.editing.clear_formats", json!({})),
        ("data.sort", json!({"keys": [{"column": "A"}]})),
        ("edit.undo", json!({})),
        ("nav.end_sheet", json!({})),
        ("select.range", json!({"range": "A1"})),
    ] {
        let t = std::time::Instant::now();
        let r = a.try_ex(id, p);
        let secs = t.elapsed().as_secs_f64();
        c.check(
            &format!("{id} on the whole sheet returns within 10 s"),
            secs < 10.0 && a.alive(),
            (secs, r.err()),
        );
    }
    let r = a.raw(json!({"op": "exec", "id": "no.such.command"}))?;
    c.check(
        "an unknown command is an error, not a crash",
        r["ok"] == json!(false) && a.alive(),
        &r,
    );
    let r = a.raw(json!({"op": "nonsense"}))?;
    c.check("an unknown op is an error", r["ok"] == json!(false), &r);
    let r = a.raw(json!({"op": "get", "range": "A1:XFD1048576"}));
    c.check(
        "getting the whole grid does not crash",
        r.is_ok() && a.alive(),
        r.err().map(|e| e.to_string()),
    );
    a.quit(c, "exits cleanly");
    Ok(())
}

/// Real-window captures: the release renderer draws cells, objects and dialogs.
pub fn render(c: &mut Ctx) -> Result<()> {
    let mut a = c.launch()?;
    let blank = c.path("blank.png");
    let ok = a.screenshot(&blank);
    let size = png_size(&blank);
    let captured = ok && size.is_some_and(|(w, h)| w > 400 && h > 300);
    c.check("captures the window", captured, size);
    if !captured {
        // Every later check needs a capture; don't wait 10 s for each one.
        a.quit(c, "exits cleanly");
        return Ok(());
    }
    sales_table(&mut a, c);
    a.ex(
        c,
        "insert.chart.column",
        json!({"series": [{"values": "E2:E9", "name": "E1", "categories": "B2:B9"}], "cell": "G2"}),
    );
    a.ex(
        c,
        "insert.shapes",
        json!({"shape": "ellipse", "cell": "G20", "width": 120, "height": 60}),
    );
    let full = c.path("content.png");
    let ok = a.screenshot(&full);
    let (b0, b1) = (
        std::fs::read(&blank).unwrap_or_default(),
        std::fs::read(&full).unwrap_or_default(),
    );
    c.check(
        "content changes the rendered window",
        ok && !b1.is_empty() && b0 != b1,
        (b0.len(), b1.len()),
    );
    // Every view mode renders (each capture forces a real frame).
    for (id, p) in [
        ("view.page_break_preview", json!({})),
        ("view.page_layout", json!({})),
        ("view.normal", json!({})),
        ("view.zoom", json!({"percent": 400})),
        ("view.zoom", json!({"percent": 10})),
        ("view.zoom_100", json!({})),
        ("view.freeze_panes", json!({"cell": "C3"})),
        ("view.split", json!({})),
        ("view.full_screen", json!({})),
        ("view.full_screen", json!({})),
        ("view.gridlines", json!({})),
        ("view.headings", json!({})),
        ("nav.end_sheet", json!({})),
    ] {
        let r = a.try_ex(id, p.clone());
        let shot = c.path(&format!("{}-{}.png", id.replace('.', "_"), p["percent"]));
        let ok = a.screenshot(&shot);
        c.check(
            &format!("{id} {p} renders"),
            r.is_ok() && ok && a.alive(),
            r.err(),
        );
    }
    a.quit(c, "exits cleanly");
    Ok(())
}
