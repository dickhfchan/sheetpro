//! Every worksheet function, typed into cells through the real shell (the formula-entry
//! path a user takes: type into the active cell, press Enter), checked against a known
//! result.
//!
//! The cases live in `tests/data/function_cases.tsv`, one per line:
//! `formula<TAB>expected<TAB>source`. Expected values are written the way the property
//! snapshot prints them: numbers plain, text JSON-quoted, `TRUE`/`FALSE`, errors by code
//! (`#DIV/0!`); `x ±t` for an iterative result (IRR, XIRR…) whose last digits depend on
//! where the iteration stops. Two sources only:
//! - `corpus <file>!<cell>`: the cached result in a real file saved by the target app (the
//!   oracle corpus). Cell references in the original formula are replaced by the referenced
//!   constants (single cells inline, ranges as array constants), only where that cannot
//!   change the meaning (see the notes at the top of the TSV).
//! - `def: …`: follows from the function's documented definition (the arithmetic is shown
//!   when not obvious).
//!
//! Never from what SheetPro returns. Volatile functions get property checks instead
//! (`?name` expectations, see [`check_special`]).
//!
//! Some cases need cells to point at: [`DATA`] is typed into `J1:M10` first (through the UI
//! too), and a pivot of it at `O1` for GETPIVOTDATA. Cases go down column A, one per row.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
use egui_kittest::Harness;
use serde_json::json;
use sp_core::{CellAddr, Value};
use sp_engine::Session;
use sp_ui::App;
use std::collections::BTreeSet;

mod common;
use common::props::{click_cell, press};

const CASES: &str = include_str!("data/function_cases.tsv");

/// Cells the reference cases point at, typed before the cases (cell, input).
const DATA: [(&str, &str); 30] = [
    ("J1", "Name"),
    ("K1", "Qty"),
    ("L1", "Price"),
    ("M1", "Note"),
    ("J2", "Apple"),
    ("K2", "10"),
    ("L2", "1.5"),
    ("M2", "x"),
    ("J3", "Pear"),
    ("K3", "4"),
    ("L3", "2"),
    ("J4", "Apple"),
    ("K4", "7"),
    ("L4", "3"),
    ("M4", "TRUE"),
    ("J5", "Plum"),
    ("K5", "12"),
    ("L5", "0.5"),
    ("J6", "Apple"),
    ("K6", "3"),
    ("L6", "4"),
    ("J7", "Fig"),
    ("K7", "=K2*2"),
    ("L7", "1"),
    // Criteria ranges for the D-functions.
    ("J9", "Name"),
    ("J10", "Apple"),
    ("K9", "Qty"),
    ("K10", ">5"),
    ("L9", "Name"),
    ("L10", "Pear"),
];

/// Functions with no passing case, each with the reason (excluded from the run's
/// expectations and reported).
const KNOWN_GAPS: [(&str, &str); 0] = [];

/// Functions a user cannot type (the target refuses them), covered elsewhere.
const NOT_TYPED: [(&str, &str); 1] = [(
    "TABLE",
    "the what-if data-table formula {=TABLE(r,c)}: written by Data › What-If Analysis › \
     Data Table (props_b.rs), refused when typed",
)];

/// Coverage may only grow.
const FUNCTIONS_FLOOR: usize = 504;

struct Case {
    formula: String,
    want: String,
    source: String,
    line: usize,
}

fn cases() -> Vec<Case> {
    CASES
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|(i, l)| {
            let mut p = l.splitn(3, '\t');
            let formula = p.next().unwrap_or("").to_string();
            let want = p.next().unwrap_or("").to_string();
            let source = p.next().unwrap_or("").to_string();
            assert!(
                !want.is_empty() && !source.is_empty(),
                "line {}: needs formula, expected, source",
                i + 1
            );
            Case {
                formula,
                want,
                source,
                line: i + 1,
            }
        })
        .collect()
}

/// Function names a formula calls (`NAME(`), upper case.
fn called(formula: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_str = false;
    let mut word = String::new();
    for ch in formula.chars() {
        if ch == '"' {
            in_str = !in_str;
            word.clear();
            continue;
        }
        if in_str {
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            word.push(ch);
        } else {
            if ch == '(' && word.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                out.insert(word.to_ascii_uppercase());
            }
            word.clear();
        }
    }
    out
}

fn registered() -> Vec<String> {
    let mut s = Session::new();
    let (r, _) = sp_engine::control::handle(&mut s, &json!({"op": "functions"}));
    r["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// A number from the case file, compared with a little room for the last bits of a
/// different (equally valid) evaluation order.
fn close(got: f64, want: f64) -> bool {
    if got == want {
        return true;
    }
    let tol = 1e-9 * want.abs();
    (got - want).abs() <= tol.max(1e-12)
}

fn show(v: &Value) -> String {
    match v {
        Value::Number(n) => format!("{n:?}"),
        Value::Text(t) => format!("{:?}", &**t),
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Value::Error(e) => e.as_str().to_string(),
        Value::Empty => "(empty)".into(),
    }
}

/// Checks for results that cannot be pinned (volatile functions).
fn check_special(name: &str, v: &Value) -> Result<(), String> {
    let num = match v {
        Value::Number(n) => *n,
        other => return Err(format!("expected a number, got {}", show(other))),
    };
    let ok = match name {
        // RAND(): 0 <= x < 1.
        "?unit" => (0.0..1.0).contains(&num),
        // RANDBETWEEN(1,6): an integer in [1, 6].
        "?die" => num.fract() == 0.0 && (1.0..=6.0).contains(&num),
        // TODAY(): a whole date serial after 2024-01-01 (45292) and before 2100 (73051).
        "?date" => num.fract() == 0.0 && (45292.0..73051.0).contains(&num),
        // NOW(): the same range, with a time of day.
        "?datetime" => (45292.0..73051.0).contains(&num),
        _ => return Err(format!("unknown check {name}")),
    };
    if ok {
        Ok(())
    } else {
        Err(format!("{num} fails {name}"))
    }
}

fn matches(want: &str, v: &Value) -> Result<(), String> {
    if want.starts_with('?') {
        return check_special(want, v);
    }
    // `x ±t`: an iterative result, any value within t of x (the target's own tolerance).
    if let Some((x, t)) = want.split_once(" ±") {
        let (x, t) = (x.parse::<f64>().unwrap(), t.parse::<f64>().unwrap());
        return match v {
            Value::Number(n) if (n - x).abs() <= t => Ok(()),
            other => Err(format!("expected {want}, got {}", show(other))),
        };
    }
    let ok = match v {
        Value::Number(n) => want.parse::<f64>().is_ok_and(|w| close(*n, w)),
        Value::Text(t) => {
            want.starts_with('"') && serde_json::from_str::<String>(want).is_ok_and(|w| w == **t)
        }
        Value::Bool(b) => want == if *b { "TRUE" } else { "FALSE" },
        Value::Error(e) => want == e.as_str(),
        Value::Empty => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("expected {want}, got {}", show(v)))
    }
}

/// Type `text` into the active cell and press Enter, as a user does. Returns the cell it
/// went into. A formula the shell refuses keeps the editor open: Escape, and report it.
fn type_in(h: &mut Harness<'_, App>, text: &str) -> Result<CellAddr, String> {
    let at = h.state().session.sel.active;
    h.event(egui::Event::Text(text.to_string()));
    h.run_steps(1);
    press(h, "Enter")?;
    if h.state().session.view.editing {
        let why = h.state().last_error().unwrap_or_default();
        press(h, "Escape")?;
        // Back on the cell, ready for the next case.
        h.state_mut().session.select(at);
        h.run_steps(1);
        return Err(format!("the shell refused the formula: {why}"));
    }
    Ok(at)
}

/// Type every case down column A in one shell session; each case's outcome, in order.
fn run_cases(cases: &[Case]) -> Vec<Result<(), String>> {
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(1200.0, 700.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), App::new(Session::new()));
    h.run_steps(3);
    for (cell, input) in DATA {
        click_cell(&mut h, cell);
        type_in(&mut h, input).unwrap();
    }
    // GETPIVOTDATA needs a pivot: Qty by Name, at O1 (built by its command; the pivot is
    // context here, the function is what's typed).
    h.state_mut()
        .session
        .execute(
            "insert.pivot_table",
            &json!({"source": "J1:L7", "dest": "Sheet1!O1", "rows": ["Name"],
                    "values": [{"field": "Qty"}]}),
        )
        .unwrap();
    h.run_steps(2);
    click_cell(&mut h, "A1");
    let placed: Vec<Result<CellAddr, String>> = cases
        .iter()
        .map(|c| type_in(&mut h, &format!("={}", c.formula)))
        .collect();
    let sh = h.state().session.wb.sheet(0).unwrap();
    placed
        .into_iter()
        .zip(cases)
        .map(|(r, c)| r.and_then(|at| matches(&c.want, sh.value(at))))
        .collect()
}

fn gap_for(c: &Case) -> Option<&'static str> {
    let calls = called(&c.formula);
    KNOWN_GAPS
        .iter()
        .find(|(f, _)| calls.contains(*f))
        .map(|(f, _)| *f)
}

#[test]
fn every_function_case_gives_its_known_result() {
    let t = std::time::Instant::now();
    let cases = cases();
    let out = run_cases(&cases);
    let mut failed = Vec::new();
    for (c, r) in cases.iter().zip(&out) {
        if let Err(why) = r {
            let gap = gap_for(c);
            println!(
                "{}  line {:>4}  ={}   {why}   [{}]",
                if gap.is_some() { "gap " } else { "FAIL" },
                c.line,
                c.formula,
                c.source
            );
            if gap.is_none() {
                failed.push(c.line);
            }
        }
    }
    println!(
        "functions: {} of {} cases pass ({:?})",
        out.iter().filter(|r| r.is_ok()).count(),
        out.len(),
        t.elapsed()
    );
    assert!(
        failed.is_empty(),
        "{} cases fail: lines {failed:?}",
        failed.len()
    );
}

#[test]
fn every_registered_function_has_a_case() {
    let cases = cases();
    let mut covered = BTreeSet::new();
    for c in &cases {
        covered.extend(called(&c.formula));
    }
    let names = registered();
    let gaps: BTreeSet<&str> = KNOWN_GAPS
        .iter()
        .chain(NOT_TYPED.iter())
        .map(|(f, _)| *f)
        .collect();
    let missing: Vec<&String> = names
        .iter()
        .filter(|n| !covered.contains(*n) && !gaps.contains(n.as_str()))
        .collect();
    let have = names.iter().filter(|n| covered.contains(*n)).count();
    println!("functions with cases: {have} of {}", names.len());
    assert!(
        missing.is_empty(),
        "{} functions have no case: {missing:?}",
        missing.len()
    );
    assert!(have >= FUNCTIONS_FLOOR, "{have} < floor {FUNCTIONS_FLOOR}");
}
