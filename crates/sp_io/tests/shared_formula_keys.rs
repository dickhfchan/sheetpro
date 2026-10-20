//! Every formula of the oracle corpus: its relative key (`sp_formula::shared`) must agree with
//! the parser, and instantiating the key elsewhere must parse to the formula moved there.
//! Skipped when the corpus has not been fetched (`cargo xtask corpus`).

use sp_core::CellAddr;
use sp_formula::shared::{KeyKind, instantiate, rel_key, verify};
use std::path::{Path, PathBuf};

fn corpus_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "xlsx") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn corpus_formulas_share_exactly() {
    let mut checked = 0usize;
    let mut failures = Vec::new();
    for path in corpus_files() {
        let Ok(f) = std::fs::File::open(&path) else {
            continue;
        };
        let Ok((wb, _)) = sp_io::xlsx_read::read_xlsx(std::io::BufReader::new(f)) else {
            continue;
        };
        for s in &wb.sheets {
            for (a, c) in s.iter() {
                let Some(src) = c.formula_src() else { continue };
                let mut k = String::new();
                let kind = rel_key(src, a, &mut k);
                let Ok(e) = sp_formula::parse(src) else {
                    continue;
                };
                if kind == KeyKind::Literal {
                    continue;
                }
                checked += 1;
                let v = verify(&e, &k, a);
                if !v.shareable {
                    failures.push(format!("{}: {src} does not match its key", path.display()));
                    continue;
                }
                if !v.text_safe || kind != (KeyKind::Relative { rel_span: false }) {
                    continue;
                }
                for (dr, dc) in [(1u32, 0u32), (0, 1), (7, 3), (300, 0)] {
                    let to = CellAddr::new(a.row + dr, a.col + dc);
                    let mut t = String::new();
                    if !instantiate(&k, to, &mut t) {
                        continue;
                    }
                    let mut moved = e.clone();
                    sp_formula::shift::shift_for_copy(&mut moved, i64::from(dr), i64::from(dc));
                    if sp_formula::parse(&t).ok() != Some(moved) {
                        failures.push(format!("{}: {src} moved to {t}", path.display()));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{checked} checked:\n{}",
        failures.join("\n")
    );
}
