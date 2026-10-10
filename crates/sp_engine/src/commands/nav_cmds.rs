//! Keyboard navigation and selection (`nav.*`, `select.*`).

use crate::params::range;
use crate::{CmdResult, CommandSpec, Selection, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, MAX_COLS, MAX_ROWS};

#[derive(Clone, Copy)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

fn step(a: CellAddr, d: Dir, n: u32) -> CellAddr {
    match d {
        Dir::Up => CellAddr::new(a.row.saturating_sub(n), a.col),
        Dir::Down => CellAddr::new(a.row.saturating_add(n).min(MAX_ROWS - 1), a.col),
        Dir::Left => CellAddr::new(a.row, a.col.saturating_sub(n)),
        Dir::Right => CellAddr::new(a.row, a.col.saturating_add(n).min(MAX_COLS - 1)),
    }
}

fn filled(s: &Session, a: CellAddr) -> bool {
    s.wb.sheet(s.sheet_index())
        .and_then(|sh| sh.get(a))
        .is_some_and(|c| !c.value.is_empty())
}

fn hidden(s: &Session, a: CellAddr) -> bool {
    s.wb.sheet(s.sheet_index())
        .is_some_and(|sh| sh.hidden_rows.contains(&a.row) || sh.hidden_cols.contains(&a.col))
}

/// One step, skipping hidden rows/columns.
fn step_visible(s: &Session, a: CellAddr, d: Dir) -> CellAddr {
    let mut cur = a;
    for _ in 0..100_000 {
        let next = step(cur, d, 1);
        if next == cur {
            return a;
        }
        if !hidden(s, next) {
            return next;
        }
        cur = next;
    }
    a
}

/// Ctrl+Arrow: jump to the edge of the current data region, or to the next filled cell, or
/// to the sheet edge.
fn edge(s: &Session, a: CellAddr, d: Dir) -> CellAddr {
    let Some(sh) = s.wb.sheet(s.sheet_index()) else {
        return a;
    };
    let next = step(a, d, 1);
    if next == a {
        return a;
    }
    if filled(s, a) && filled(s, next) {
        let mut cur = next;
        loop {
            let n = step(cur, d, 1);
            if n == cur || !filled(s, n) {
                return cur;
            }
            cur = n;
        }
    }
    // Find the next filled cell along the line using sparse storage.
    let found = match d {
        Dir::Down => sh
            .cells
            .range(a.row + 1..)
            .find(|(_, r)| r.get(&a.col).is_some_and(|c| !c.value.is_empty()))
            .map(|(r, _)| CellAddr::new(*r, a.col)),
        Dir::Up => sh
            .cells
            .range(..a.row)
            .rev()
            .find(|(_, r)| r.get(&a.col).is_some_and(|c| !c.value.is_empty()))
            .map(|(r, _)| CellAddr::new(*r, a.col)),
        Dir::Right => sh.cells.get(&a.row).and_then(|r| {
            r.range(a.col + 1..)
                .find(|(_, c)| !c.value.is_empty())
                .map(|(c, _)| CellAddr::new(a.row, *c))
        }),
        Dir::Left => sh.cells.get(&a.row).and_then(|r| {
            r.range(..a.col)
                .rev()
                .find(|(_, c)| !c.value.is_empty())
                .map(|(c, _)| CellAddr::new(a.row, *c))
        }),
    };
    found.unwrap_or(match d {
        Dir::Down => CellAddr::new(MAX_ROWS - 1, a.col),
        Dir::Up => CellAddr::new(0, a.col),
        Dir::Right => CellAddr::new(a.row, MAX_COLS - 1),
        Dir::Left => CellAddr::new(a.row, 0),
    })
}

fn move_to(s: &mut Session, a: CellAddr) -> CmdResult {
    s.select(a);
    Ok(json!(a.to_a1()))
}

fn extend_to(s: &mut Session, cursor: CellAddr) -> CmdResult {
    s.sel = Selection {
        active: s.sel.active,
        anchor: s.sel.anchor,
        cursor,
    };
    s.ensure_visible(cursor);
    Ok(json!(s.sel.range().to_a1()))
}

/// Enter/Tab: commit and move, wrapping inside a multi-cell selection.
fn move_in_selection(s: &mut Session, down: bool) -> CmdResult {
    let r = s.sel.range();
    if r.is_single() {
        let a = step_visible(s, s.sel.active, if down { Dir::Down } else { Dir::Right });
        return move_to(s, a);
    }
    let a = s.sel.active;
    let next = if down {
        if a.row < r.end.row {
            CellAddr::new(a.row + 1, a.col)
        } else if a.col < r.end.col {
            CellAddr::new(r.start.row, a.col + 1)
        } else {
            r.start
        }
    } else if a.col < r.end.col {
        CellAddr::new(a.row, a.col + 1)
    } else if a.row < r.end.row {
        CellAddr::new(a.row + 1, r.start.col)
    } else {
        r.start
    };
    s.sel.active = next;
    s.ensure_visible(next);
    Ok(json!(next.to_a1()))
}

/// Contiguous non-empty block around `a` (Ctrl+Shift+*).
pub fn current_region(s: &Session, a: CellAddr) -> CellRange {
    let mut r = CellRange::single(a);
    loop {
        let grown = CellRange::new(
            CellAddr::new(r.start.row.saturating_sub(1), r.start.col.saturating_sub(1)),
            CellAddr::new(
                (r.end.row + 1).min(MAX_ROWS - 1),
                (r.end.col + 1).min(MAX_COLS - 1),
            ),
        );
        let Some(sh) = s.wb.sheet(s.sheet_index()) else {
            return r;
        };
        let mut nr = r;
        for (addr, c) in sh.cells_in(grown) {
            if !c.value.is_empty() && !r.contains(addr) {
                nr = CellRange::new(
                    CellAddr::new(nr.start.row.min(addr.row), nr.start.col.min(addr.col)),
                    CellAddr::new(nr.end.row.max(addr.row), nr.end.col.max(addr.col)),
                );
            }
        }
        if nr == r || nr.cell_count() > sp_core::MAX_CELLS_PER_OP {
            return r;
        }
        r = nr;
    }
}

fn page(s: &mut Session, d: Dir) -> CmdResult {
    let n = match d {
        Dir::Up | Dir::Down => s.view.page_rows.max(1),
        Dir::Left | Dir::Right => s.view.page_cols.max(1),
    };
    let a = step(s.sel.active, d, n);
    let sc = step(s.view.scroll, d, n);
    s.view.scroll = sc;
    move_to(s, a)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("nav.move_up", "{}", |s, _| {
            move_to(s, step_visible(s, s.sel.active, Dir::Up))
        }),
        CommandSpec::new("nav.move_down", "{}", |s, _| {
            move_to(s, step_visible(s, s.sel.active, Dir::Down))
        }),
        CommandSpec::new("nav.move_left", "{}", |s, _| {
            move_to(s, step_visible(s, s.sel.active, Dir::Left))
        }),
        CommandSpec::new("nav.move_right", "{}", |s, _| {
            move_to(s, step_visible(s, s.sel.active, Dir::Right))
        }),
        CommandSpec::new("nav.edge_up", "{}", |s, _| {
            move_to(s, edge(s, s.sel.active, Dir::Up))
        }),
        CommandSpec::new("nav.edge_down", "{}", |s, _| {
            move_to(s, edge(s, s.sel.active, Dir::Down))
        }),
        CommandSpec::new("nav.edge_left", "{}", |s, _| {
            move_to(s, edge(s, s.sel.active, Dir::Left))
        }),
        CommandSpec::new("nav.edge_right", "{}", |s, _| {
            move_to(s, edge(s, s.sel.active, Dir::Right))
        }),
        CommandSpec::new("nav.home_row", "{}", |s, _| {
            move_to(s, CellAddr::new(s.sel.active.row, 0))
        }),
        CommandSpec::new("nav.home_sheet", "{}", |s, _| {
            let f =
                s.wb.sheet(s.sheet_index())
                    .and_then(|sh| sh.freeze)
                    .unwrap_or_default();
            move_to(s, f)
        }),
        CommandSpec::new("nav.end_sheet", "{}", |s, _| {
            let end =
                s.wb.sheet(s.sheet_index())
                    .and_then(|sh| sh.used_range())
                    .map_or(CellAddr::default(), |r| r.end);
            move_to(s, end)
        }),
        CommandSpec::new("nav.page_down", "{}", |s, _| page(s, Dir::Down)),
        CommandSpec::new("nav.page_up", "{}", |s, _| page(s, Dir::Up)),
        CommandSpec::new("nav.page_right", "{}", |s, _| page(s, Dir::Right)),
        CommandSpec::new("nav.page_left", "{}", |s, _| page(s, Dir::Left)),
        CommandSpec::new("nav.enter_next", "{}", |s, _| move_in_selection(s, true)),
        CommandSpec::new("nav.tab_next", "{}", |s, _| move_in_selection(s, false)),
        CommandSpec::new("select.extend_up", "{}", |s, _| {
            extend_to(s, step(s.sel.cursor, Dir::Up, 1))
        }),
        CommandSpec::new("select.extend_down", "{}", |s, _| {
            extend_to(s, step(s.sel.cursor, Dir::Down, 1))
        }),
        CommandSpec::new("select.extend_left", "{}", |s, _| {
            extend_to(s, step(s.sel.cursor, Dir::Left, 1))
        }),
        CommandSpec::new("select.extend_right", "{}", |s, _| {
            extend_to(s, step(s.sel.cursor, Dir::Right, 1))
        }),
        CommandSpec::new("select.extend_edge_up", "{}", |s, _| {
            extend_to(s, edge(s, s.sel.cursor, Dir::Up))
        }),
        CommandSpec::new("select.extend_edge_down", "{}", |s, _| {
            extend_to(s, edge(s, s.sel.cursor, Dir::Down))
        }),
        CommandSpec::new("select.extend_edge_left", "{}", |s, _| {
            extend_to(s, edge(s, s.sel.cursor, Dir::Left))
        }),
        CommandSpec::new("select.extend_edge_right", "{}", |s, _| {
            extend_to(s, edge(s, s.sel.cursor, Dir::Right))
        }),
        CommandSpec::new("select.all", "{}", |s, _| {
            let region = current_region(s, s.sel.active);
            // First press selects the current region; pressing again selects the sheet.
            let r = if region.is_single() || s.sel.range() == region {
                CellRange::new(
                    CellAddr::new(0, 0),
                    CellAddr::new(MAX_ROWS - 1, MAX_COLS - 1),
                )
            } else {
                region
            };
            s.sel = Selection {
                active: s.sel.active,
                anchor: r.start,
                cursor: r.end,
            };
            Ok(json!(r.to_a1()))
        }),
        CommandSpec::new("select.row", "{}", |s, _| {
            let row = s.sel.active.row;
            s.sel = Selection {
                active: s.sel.active,
                anchor: CellAddr::new(s.sel.range().start.row.min(row), 0),
                cursor: CellAddr::new(s.sel.range().end.row.max(row), MAX_COLS - 1),
            };
            Ok(json!(s.sel.range().to_a1()))
        }),
        CommandSpec::new("select.column", "{}", |s, _| {
            let col = s.sel.active.col;
            s.sel = Selection {
                active: s.sel.active,
                anchor: CellAddr::new(0, s.sel.range().start.col.min(col)),
                cursor: CellAddr::new(MAX_ROWS - 1, s.sel.range().end.col.max(col)),
            };
            Ok(json!(s.sel.range().to_a1()))
        }),
        CommandSpec::new("select.current_region", "{}", |s, _| {
            let r = current_region(s, s.sel.active);
            s.sel = Selection {
                active: s.sel.active,
                anchor: r.start,
                cursor: r.end,
            };
            Ok(json!(r.to_a1()))
        }),
        CommandSpec::new(
            "select.range",
            r#"{"range": "A1:C5", "active"?: "B2"}"#,
            |s, p| {
                let (si, r) = range(s, p, "range")?;
                s.wb.active = si;
                let active = match crate::params::str_opt(p, "active") {
                    Some(t) => {
                        CellAddr::parse(t).map_err(|e| crate::CmdError::Param(e.to_string()))?
                    }
                    None => r.start,
                };
                s.select_range(r, active);
                Ok(json!(r.to_a1()))
            },
        ),
    ]
}
