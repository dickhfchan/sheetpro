//! Loading a pre-dynamic-array formula adds `@` where it intersects implicitly; saving drops
//! it again. Repeated save/load cycles must leave the formula alone (they used to add a pair
//! of parentheses around `@(…)` each time).

#![allow(clippy::unwrap_used)]
use sp_formula::{parse, to_formula};

/// File text → the formula the user sees (load).
fn load(file: &str) -> String {
    to_formula(&parse(file).unwrap().into_legacy())
}

/// The formula the user sees → file text (save).
fn save(shown: &str) -> String {
    to_formula(&parse(shown).unwrap().from_legacy().unwrap())
}

#[test]
fn save_load_cycles_are_stable() {
    for file in [
        "((C2:D3 D3:E4))",
        "C2:D3 D3:E4",
        "L6-((-4,3))",
        "(Tabelle2!E:E Tabelle2!11:11)",
        "(A1:A3)+1",
        "A1:A3",
        "SUM(A1:A3)*2",
        "-(B1:B4)",
    ] {
        let first = load(file);
        let mut shown = first.clone();
        for cycle in 0..4 {
            let again = load(&save(&shown));
            assert_eq!(
                again, shown,
                "{file}: cycle {cycle} changed {shown:?} to {again:?}"
            );
            shown = again;
        }
    }
    // The shapes the target shows.
    assert_eq!(load("((C2:D3 D3:E4))"), "(@(C2:D3 D3:E4))");
    assert_eq!(load("L6-((-4,3))"), "L6-(@(-4,3))");
    assert_eq!(load("SUM(A1:A3)*2"), "SUM(A1:A3)*2");
}
