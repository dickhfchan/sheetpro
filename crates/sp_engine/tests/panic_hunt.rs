//! panic_hunt: every registered command × adversarial params × odd session states.
//! Commands may return errors; they must never panic. Run via `cargo xtask panic-hunt`.

use serde_json::{Value as Json, json};
use sp_core::{CellAddr, CellRange, MAX_COLS, MAX_ROWS};
use sp_engine::{Selection, Session, registry};

fn adversarial_params() -> Vec<Json> {
    let huge = format!("{}{}", "A".repeat(3), u64::MAX);
    vec![
        json!({}),
        json!(null),
        json!([1, 2, 3]),
        json!({"range": "XFD1048576", "cell": "XFD1048576"}),
        json!({"range": "A:XFD", "cell": "A1"}),
        json!({"range": "1:1048576"}),
        json!({"range": "Nope!A1", "cell": "'Bad''Sheet'!A1"}),
        json!({"range": huge, "cell": "", "input": ""}),
        json!({"input": "=".repeat(3) + &"(".repeat(500), "cell": "A1"}),
        json!({"input": "=1/0", "what": "", "with": "", "reference": "A1:B2"}),
        json!({"input": "x".repeat(40_000)}),
        json!({"name": "", "size": -1, "height": f64::MAX, "width": -0.0, "percent": 1e308, "degrees": 1e9}),
        json!({"name": "a/b", "sheet": 99, "to": 4_000_000_000u64, "code": "0".repeat(300)}),
        json!({"color": "#zzzzzz", "path": "/nonexistent/dir/x.xlsx", "delimiter": ""}),
        json!({"keys": [{"column": "ZZZZ"}], "columns": ["A", 7], "source": "A1:B2", "dest": "A1"}),
        json!({"keys": [], "shift": "sideways", "sheet": "Sheet1", "refers_to": "=SUM("}),
        json!({"sheet": -1, "percent": "abc", "size": "NaN", "prefix": "\u{0}"}),
    ]
}

fn states() -> Vec<Session> {
    let mut v = Vec::new();
    v.push(Session::new());

    let mut s = Session::new();
    for (a, i) in [
        ("A1", "1"),
        ("A2", "2"),
        ("B1", "=A1+A2"),
        ("C3", "hello"),
        ("D4", "=B1/0"),
    ] {
        let _ = s.execute("cell.set", &json!({"cell": a, "input": i}));
    }
    s.select_range(
        CellRange::parse("A1:D4").unwrap_or_default(),
        CellAddr::new(0, 0),
    );
    v.push(s);

    let mut s = Session::new();
    s.sel = Selection {
        active: CellAddr::new(MAX_ROWS - 1, MAX_COLS - 1),
        anchor: CellAddr::new(0, 0),
        cursor: CellAddr::new(MAX_ROWS - 1, MAX_COLS - 1),
    };
    v.push(s);

    let mut s = Session::new();
    let _ = s.execute("home.cells.insert_sheet", &json!({"name": "Two"}));
    let _ = s.execute("home.clipboard.copy", &json!({"range": "A1:C3"}));
    let _ = s.execute("view.freeze_panes", &json!({"cell": "B2"}));
    v.push(s);
    v
}

#[test]
fn no_command_panics() {
    let params = adversarial_params();
    let mut runs = 0usize;
    for cmd in registry().all() {
        for p in &params {
            for mut s in states() {
                let _ = s.execute(cmd.id, p);
                let _ = s.execute("edit.undo", &json!({}));
                let _ = s.execute("edit.redo", &json!({}));
                runs += 1;
            }
        }
    }
    assert!(runs > 1000, "panic_hunt ran {runs} cases");
}

/// Diagnostic: per-command timing (`cargo test -p sp_engine --test panic_hunt timing -- --ignored --nocapture`).
#[test]
#[ignore]
fn timing() {
    let params = adversarial_params();
    for cmd in registry().all() {
        let t = std::time::Instant::now();
        for p in &params {
            for mut s in states() {
                let _ = s.execute(cmd.id, p);
            }
        }
        let ms = t.elapsed().as_millis();
        eprintln!("{ms:>8} ms  {}", cmd.id);
    }
}
