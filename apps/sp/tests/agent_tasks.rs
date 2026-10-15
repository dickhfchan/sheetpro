//! M12 "agents can drive SheetPro": ten realistic multi-step workflow tasks, each done only
//! through MCP tools against the real `sp mcp` binary over stdio, with an end-state
//! assertion. The scorecard row "Agent workflow tasks" counts these.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::{Value as Json, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start() -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sp"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn sp mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Client {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let init = c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "agent_tasks", "version": "1"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "sheetpro");
        c.notify("notifications/initialized");
        c
    }

    fn send(&mut self, msg: &Json) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": method}));
    }

    fn request(&mut self, method: &str, params: Json) -> Json {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let reply: Json = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    /// Call a tool; returns the raw result (content + isError).
    fn call_raw(&mut self, name: &str, args: Json) -> Json {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert!(r.get("error").is_none(), "{name}: {r}");
        r["result"].clone()
    }

    /// Call a tool that must succeed; returns its structured content.
    fn tool(&mut self, name: &str, args: Json) -> Json {
        let r = self.call_raw(name, args.clone());
        assert_eq!(r["isError"], false, "{name} {args} → {r}");
        r["structuredContent"].clone()
    }

    /// Call a tool that must fail; returns the error text.
    fn tool_err(&mut self, name: &str, args: Json) -> String {
        let r = self.call_raw(name, args.clone());
        assert_eq!(r["isError"], true, "{name} {args} should fail → {r}");
        r["content"][0]["text"].as_str().unwrap_or("").to_string()
    }

    fn run(&mut self, id: &str, params: Json) -> Json {
        self.tool("run_command", json!({"id": id, "params": params}))["result"].clone()
    }

    fn set(&mut self, range: &str, values: Json) {
        self.tool("set_cells", json!({"range": range, "values": values}));
    }

    /// Rows of cells of `range`.
    fn cells(&mut self, range: &str) -> Vec<Vec<Json>> {
        let r = self.tool("get_cells", json!({"range": range}));
        serde_json::from_value(r["rows"].clone()).unwrap()
    }

    fn value(&mut self, cell: &str) -> Json {
        self.cells(cell)[0][0]["value"].clone()
    }

    fn num(&mut self, cell: &str) -> f64 {
        let v = self.value(cell);
        v.as_f64().unwrap_or_else(|| panic!("{cell} = {v}"))
    }

    fn display(&mut self, cell: &str) -> String {
        self.cells(cell)[0][0]["display"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Values of a column range as display strings.
    fn column(&mut self, range: &str) -> Vec<String> {
        self.cells(range)
            .into_iter()
            .map(|r| r[0]["display"].as_str().unwrap_or("").to_string())
            .collect()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sp-agent-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// 1. Build a 12-month loan amortization table.
#[test]
fn task01_loan_amortization() {
    let mut c = Client::start();
    c.set(
        "A1",
        json!([
            ["Principal", 100000],
            ["Monthly rate", "=6%/12"],
            ["Months", 12],
            ["Payment", "=PMT(B2,B3,-B1)"],
            ["Period", "Payment", "Interest", "Principal", "Balance"],
            [0, null, null, null, "=B1"],
            ["=A6+1", "=$B$4", "=E6*$B$2", "=B7-C7", "=E6-D7"],
        ]),
    );
    c.run("home.editing.fill_down", json!({"range": "A7:E18"}));
    c.run("home.number.currency", json!({"range": "B4:E18"}));
    assert_eq!(c.num("A18"), 12.0);
    assert!(c.num("E18").abs() < 1e-6, "loan must be paid off");
    c.set("G1", json!([["=SUM(C7:C18)"]]));
    let interest = c.num("G1");
    assert!(close(interest, c.num("B4") * 12.0 - 100000.0));
    assert!((interest - 3279.72).abs() < 0.01, "{interest}");
    assert!(c.display("B4").contains("8,606.64"), "{}", c.display("B4"));
}

/// 2. Clean a messy imported CSV: trim names, remove duplicates, split a column.
#[test]
fn task02_clean_imported_csv() {
    let dir = temp_dir("csv");
    let csv = dir.join("import.csv");
    std::fs::write(
        &csv,
        "Name,Amount,Location\n  Alice Smith ,100,Seattle-WA\nBob Jones,250,Austin-TX\n  Alice Smith ,100,Seattle-WA\nCarol  Diaz   ,75,Boston-MA\nBob Jones,250,Austin-TX\n",
    )
    .unwrap();
    let mut c = Client::start();
    c.tool("open_workbook", json!({"path": csv}));
    // Helper column with TRIM, pasted back as values, helper cleared.
    c.run(
        "cell.fill_selection",
        json!({"range": "D2:D6", "input": "=TRIM(A2)"}),
    );
    c.run("home.clipboard.copy", json!({"range": "D2:D6"}));
    c.run("home.clipboard.paste_values", json!({"cell": "A2"}));
    c.run("home.editing.clear_all", json!({"range": "D2:D6"}));
    c.run(
        "data.remove_duplicates",
        json!({"range": "A1:C6", "header": true}),
    );
    c.run(
        "data.text_to_columns",
        json!({"range": "C2:C4", "delimiter": "-"}),
    );
    c.set("C1", json!([["City", "State"]]));
    let rows: Vec<Vec<String>> = c
        .cells("A1:D5")
        .iter()
        .map(|r| {
            r.iter()
                .map(|x| x["display"].as_str().unwrap().to_string())
                .collect()
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            vec!["Name", "Amount", "City", "State"],
            vec!["Alice Smith", "100", "Seattle", "WA"],
            vec!["Bob Jones", "250", "Austin", "TX"],
            vec!["Carol Diaz", "75", "Boston", "MA"],
            vec!["", "", "", ""],
        ]
    );
    assert_eq!(c.value("B2"), json!(100.0));
}

/// 3. Sales pivot by region plus a column chart of it; check the screenshot renders.
#[test]
fn task03_sales_pivot_and_chart() {
    let mut c = Client::start();
    c.set(
        "A1",
        json!([
            ["Region", "Product", "Sales"],
            ["East", "Widget", 120],
            ["West", "Widget", 80],
            ["East", "Gadget", 200],
            ["North", "Gadget", 50],
            ["West", "Gizmo", 70],
            ["East", "Gizmo", 30],
            ["North", "Widget", 100],
        ]),
    );
    c.run(
        "insert.pivot_table",
        json!({"source": "A1:C8", "dest": "Sheet1!E1", "rows": ["Region"], "values": [{"field": "Sales", "func": "sum"}]}),
    );
    let pivot: Vec<Vec<String>> = c
        .cells("E1:F6")
        .iter()
        .map(|r| {
            r.iter()
                .map(|x| x["display"].as_str().unwrap().to_string())
                .collect()
        })
        .collect();
    let total_row = pivot
        .iter()
        .find(|r| r[0].contains("Grand Total"))
        .unwrap_or_else(|| panic!("{pivot:?}"));
    assert_eq!(total_row[1], "650");
    let east = pivot.iter().find(|r| r[0] == "East").unwrap();
    assert_eq!(east[1], "350");
    let chart = c.run(
        "insert.chart.column",
        json!({"range": "E2:F4", "title": "Sales by Region", "anchor": "H2:O17"}),
    );
    assert!(!chart.is_null());
    // Saved file carries the chart.
    let dir = temp_dir("pivot");
    let out = dir.join("sales.xlsx");
    c.tool("save_workbook", json!({"path": out}));
    let bytes = std::fs::read(&out).unwrap();
    assert!(
        bytes
            .windows(b"xl/charts/chart".len())
            .any(|w| w == b"xl/charts/chart"),
        "no chart part"
    );
    // Screenshot through MCP: PNG image content.
    let shot = c.call_raw("screenshot", json!({"width": 800, "height": 600}));
    assert_eq!(shot["isError"], false, "{shot}");
    assert_eq!(shot["content"][0]["type"], "image");
    assert_eq!(shot["content"][0]["mimeType"], "image/png");
    assert!(
        shot["content"][0]["data"]
            .as_str()
            .unwrap()
            .starts_with("iVBORw0KGgo")
    );
}

/// 4. Conditional-format overdue, unpaid invoices.
#[test]
fn task04_conditional_format_overdue_invoices() {
    let mut c = Client::start();
    c.set(
        "A1",
        json!([
            ["Invoice", "Amount", "Due", "Paid"],
            ["INV-1", 500, "=TODAY()-30", "No"],
            ["INV-2", 300, "=TODAY()-5", "Yes"],
            ["INV-3", 900, "=TODAY()+10", "No"],
            ["INV-4", 150, "=TODAY()-1", "No"],
        ]),
    );
    c.run("home.number.date_short", json!({"range": "C2:C5"}));
    let rule = c.run(
        "home.styles.cf.new_rule",
        json!({"range": "A2:D5", "type": "formula", "formula": "=AND($C2<TODAY(),$D2=\"No\")", "format": {"fill": "FFC7CE", "font_color": "9C0006"}}),
    );
    assert!(!rule.is_null());
    let rules = c.run("home.styles.cf.manage", json!({"action": "list"}));
    let list = rules
        .as_array()
        .cloned()
        .unwrap_or_else(|| rules["rules"].as_array().cloned().unwrap_or_default());
    assert_eq!(list.len(), 1, "{rules}");
    assert!(list[0].to_string().contains("A2:D5"), "{rules}");
    c.set(
        "F1",
        json!([
            ["Overdue", "=COUNTIFS(C2:C5,\"<\"&TODAY(),D2:D5,\"No\")"],
            [
                "Overdue $",
                "=SUMIFS(B2:B5,C2:C5,\"<\"&TODAY(),D2:D5,\"No\")"
            ]
        ]),
    );
    assert_eq!(c.num("G1"), 2.0);
    assert_eq!(c.num("G2"), 650.0);
}

/// 5. Budget vs actual with variance and variance %.
#[test]
fn task05_budget_vs_actual() {
    let mut c = Client::start();
    c.set(
        "A1",
        json!([
            ["Category", "Budget", "Actual", "Variance", "Variance %"],
            ["Rent", 2000, 2000],
            ["Payroll", 10000, 11500],
            ["Marketing", 3000, 2400],
            ["Travel", 0, 350],
            ["Total", "=SUM(B2:B5)", "=SUM(C2:C5)"],
        ]),
    );
    c.set("D2", json!([["=C2-B2", "=IFERROR(D2/B2,\"n/a\")"]]));
    c.run("home.editing.fill_down", json!({"range": "D2:E6"}));
    c.run("home.number.percent", json!({"range": "E2:E6"}));
    c.run("home.font.bold", json!({"range": "A6:E6"}));
    assert_eq!(c.num("D6"), 1250.0);
    assert_eq!(c.display("E3"), "15%");
    assert_eq!(c.display("E4"), "-20%");
    assert_eq!(c.value("E5"), json!("n/a"));
    assert!(close(c.num("E6"), 1250.0 / 15000.0));
}

/// 6. Sort a list, filter it, and add subtotals.
#[test]
fn task06_sort_filter_subtotal() {
    let mut c = Client::start();
    c.set(
        "A1",
        json!([
            ["Dept", "Name", "Salary"],
            ["Sales", "Ann", 50],
            ["Eng", "Bo", 90],
            ["Ops", "Cy", 40],
            ["Eng", "Di", 110],
            ["Sales", "Ed", 65],
            ["Ops", "Flo", 45],
        ]),
    );
    c.run(
        "data.sort",
        json!({"range": "A1:C7", "header": true, "keys": [{"column": "A", "order": "asc"}, {"column": "C", "order": "desc"}]}),
    );
    assert_eq!(
        c.column("B2:B7"),
        vec!["Di", "Bo", "Flo", "Cy", "Ed", "Ann"]
    );
    // Filter to Eng: SUBTOTAL(109) sees visible rows only.
    c.run(
        "data.filter",
        json!({"range": "A1:C7", "on": true, "column": "A", "values": ["Eng"]}),
    );
    c.set("E1", json!([["=SUBTOTAL(109,C2:C7)"]]));
    assert_eq!(c.num("E1"), 200.0);
    c.run("data.filter", json!({"range": "A1:C7", "on": false}));
    assert_eq!(c.num("E1"), 400.0);
    c.run("home.editing.clear_all", json!({"range": "E1"}));
    c.run(
        "data.subtotal",
        json!({"range": "A1:C7", "by": "A", "function": "sum", "columns": ["C"]}),
    );
    let names = c.column("A1:A11");
    assert!(names.iter().any(|n| n.contains("Eng Total")), "{names:?}");
    assert!(names.iter().any(|n| n.contains("Grand Total")), "{names:?}");
    let row = names
        .iter()
        .position(|n| n.contains("Grand Total"))
        .unwrap();
    assert_eq!(c.num(&format!("C{}", row + 1)), 400.0);
    let eng = names.iter().position(|n| n.contains("Eng Total")).unwrap();
    assert_eq!(c.num(&format!("C{}", eng + 1)), 200.0);
}

/// 7. Look up prices on another sheet with XLOOKUP.
#[test]
fn task07_xlookup_across_sheets() {
    let mut c = Client::start();
    c.run("home.cells.rename_sheet", json!({"name": "Orders"}));
    c.run("home.cells.insert_sheet", json!({"name": "Prices"}));
    c.set(
        "Prices!A1",
        json!([["SKU", "Price"], ["A-1", 2.5], ["B-2", 10], ["C-3", 7.25]]),
    );
    let sheets = c.tool("list_sheets", json!({}));
    let names: Vec<&str> = sheets["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Orders") && names.contains(&"Prices"),
        "{names:?}"
    );
    c.set(
        "Orders!A1",
        json!([
            ["SKU", "Qty", "Price", "Total"],
            [
                "B-2",
                3,
                "=XLOOKUP(A2,Prices!$A$2:$A$4,Prices!$B$2:$B$4,\"missing\")",
                "=IFERROR(B2*C2,0)"
            ],
            ["A-1", 4],
            ["Z-9", 1],
            ["C-3", 2],
        ]),
    );
    c.run("home.editing.fill_down", json!({"range": "Orders!C2:D5"}));
    c.set("Orders!D6", json!([["=SUM(D2:D5)"]]));
    assert_eq!(c.value("Orders!C4"), json!("missing"));
    assert_eq!(c.num("Orders!D6"), 30.0 + 10.0 + 0.0 + 14.5);
}

/// 8. Data-validation dropdown on an input column of a protected sheet.
#[test]
fn task08_validation_dropdown_protected_sheet() {
    let mut c = Client::start();
    c.set(
        "A1",
        json!([["Task", "Status"], ["Write spec", "Open"], ["Ship", "Open"]]),
    );
    c.run(
        "data.validation",
        json!({"range": "B2:B10", "type": "list", "source": "Open,In progress,Closed", "error": "Pick a status from the list"}),
    );
    c.run("home.cells.lock_cell", json!({"range": "B2:B10"}));
    c.run(
        "review.protect_sheet",
        json!({"protect": true, "password": "pw"}),
    );
    // Allowed: a listed value in an unlocked cell.
    c.run("cell.set", json!({"cell": "B3", "input": "Closed"}));
    assert_eq!(c.value("B3"), json!("Closed"));
    // Refused: a value outside the list, and any edit to a locked cell.
    let e = c.tool_err(
        "run_command",
        json!({"id": "cell.set", "params": {"cell": "B2", "input": "Maybe"}}),
    );
    assert!(e.contains("Pick a status"), "{e}");
    c.tool_err("set_cells", json!({"range": "A2", "values": [["Hacked"]]}));
    assert_eq!(c.value("A2"), json!("Write spec"));
    assert_eq!(c.value("B2"), json!("Open"));
    let rule = c.run("data.validation", json!({"range": "B2"}));
    assert!(rule.to_string().contains("Closed"), "{rule}");
}

/// 9. Print setup (landscape, print area, titles, fit to width) and PDF export.
#[test]
fn task09_print_setup_pdf_export() {
    let mut c = Client::start();
    let mut rows = vec![json!(["Item", "Q1", "Q2", "Q3", "Q4", "Total"])];
    for i in 1..=120 {
        rows.push(json!([
            format!("Item {i}"),
            i,
            i * 2,
            i * 3,
            i * 4,
            format!("=SUM(B{0}:E{0})", i + 1)
        ]));
    }
    c.set("A1", Json::Array(rows));
    c.run("layout.orientation", json!({"orientation": "landscape"}));
    c.run("layout.print_area.set", json!({"range": "A1:F121"}));
    c.run("layout.print_titles", json!({"rows": "1:1"}));
    c.run(
        "layout.scale_to_fit",
        json!({"width": 1, "height": "automatic"}),
    );
    let dir = temp_dir("pdf");
    let pdf = dir.join("report.pdf");
    c.run("file.export.pdf", json!({"path": pdf}));
    let bytes = std::fs::read(&pdf).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    let text = String::from_utf8_lossy(&bytes);
    let pages = text.matches("/Type /Page\n").count()
        + text.matches("/Type /Page ").count()
        + text.matches("/Type/Page/").count();
    let pages = if pages == 0 {
        text.matches("/Type /Page").count() - text.matches("/Type /Pages").count()
    } else {
        pages
    };
    assert!(
        pages >= 2,
        "120 rows should need several pages, got {pages}"
    );
    // Repeated title row: "Item" header text appears on every page.
    assert!(
        text.matches("(Item)").count() >= pages,
        "titles not repeated"
    );
    assert_eq!(c.num("F121"), 1200.0);
}

/// 10. Find and fix a broken model (#DIV/0!, #REF!) via error checking and tracing.
#[test]
fn task10_fix_broken_model() {
    let mut c = Client::start();
    c.set(
        "A1",
        json!([
            ["Units", 100, "Scratch"],
            ["Price", 4, 1],
            ["Revenue", "=B1*B2"],
            ["Margin %", "=B6/B3"],
            ["Customers", 0],
            ["Profit", "=B3-C2*B1"],
            ["Per customer", "=B6/B5"],
            ["Check", "=C2+B2"],
        ]),
    );
    // Break it: deleting column C turns references to it into #REF!.
    c.run("home.cells.delete_columns", json!({"range": "C1"}));
    let issues = c.run("formulas.error_checking", json!({"select": true}));
    let cells: Vec<&str> = issues
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["cell"].as_str().unwrap())
        .collect();
    assert!(
        cells.contains(&"B6") && cells.contains(&"B7") && cells.contains(&"B8"),
        "{issues}"
    );
    assert!(issues.to_string().contains("#REF!"), "{issues}");
    // Trace a broken cell's precedents, then fix the formulas.
    let prec = c.run("formulas.trace_precedents", json!({"cell": "B7"}));
    assert!(prec.to_string().contains("B5"), "{prec}");
    let b6 = c.cells("B6")[0][0]["formula"].as_str().unwrap().to_string();
    assert!(b6.contains("#REF!"), "{b6}");
    c.set("B6", json!([["=B3-1*B1"]]));
    c.set("B8", json!([["=1+B2"]]));
    // With the #REF!s gone, the division by zero surfaces.
    let issues = c.run("formulas.error_checking", json!({}));
    assert_eq!(
        issues,
        json!([{"cell": "B7", "issue": "Error #DIV/0!"}]),
        "{issues}"
    );
    c.set("B7", json!([["=IF(B5=0,0,B6/B5)"]]));
    c.run("formulas.remove_arrows", json!({}));
    let issues = c.run("formulas.error_checking", json!({}));
    assert_eq!(issues, json!([]), "model still broken");
    assert_eq!(c.num("B6"), 300.0);
    assert!(close(c.num("B4"), 0.75));
    assert_eq!(c.num("B7"), 0.0);
}

/// Not one of the ten: macros and spelling are drivable over MCP too, and the protocol
/// survives hostile input on the wire.
#[test]
fn macros_spelling_and_hostile_input() {
    let mut c = Client::start();
    let tools = c.request("tools/list", json!({}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 10);
    c.run(
        "view.macros",
        json!({"action": "record_start", "name": "Header"}),
    );
    c.set("A1", json!([["Region", "Sales"]]));
    c.run("home.font.bold", json!({"range": "A1:B1"}));
    let r = c.run("view.macros", json!({"action": "record_stop"}));
    assert_eq!(r["steps"], 1, "set_cells is not a command; bold is");
    c.run(
        "cell.set",
        json!({"cell": "A3", "input": "Teh qiuck report"}),
    );
    let sp = c.run("review.spelling", json!({"range": "A3"}));
    let words: Vec<&str> = sp["misspellings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["word"].as_str().unwrap())
        .collect();
    assert_eq!(words, vec!["Teh", "qiuck"]);
    assert_eq!(sp["misspellings"][1]["suggestions"][0], "quick");
    c.run(
        "review.spelling",
        json!({"action": "change", "cell": "A3", "word": "qiuck", "to": "quick"}),
    );
    assert_eq!(c.value("A3"), json!("Teh quick report"));
    // Hostile wire input: garbage, wrong types, unknown methods; the server keeps going.
    c.send(&json!("hello"));
    let mut line = String::new();
    c.stdout.read_line(&mut line).unwrap();
    assert!(line.contains("-32600"), "{line}");
    writeln!(c.stdin, "{{{{{{ not json").unwrap();
    c.stdin.flush().unwrap();
    line.clear();
    c.stdout.read_line(&mut line).unwrap();
    assert!(line.contains("-32700"), "{line}");
    let r = c.request("no/such", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    c.tool_err("get_cells", json!({"range": "A1:XFD1048576"}));
    assert_eq!(c.request("ping", json!({}))["result"], json!({}));
}
