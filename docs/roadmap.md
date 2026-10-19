# Roadmap

Status legend: ✅ done · 🟡 in progress · ⬜ not started. Updated 2026-10-19.

**Parity metrics.** `cargo xtask parity` measures how much of the target's catalog (ribbon /
keyboard commands and worksheet functions) is *wired* (`parity.md`). It does **not** measure
whether those commands behave like the target or survive real files. Read it next to the
oracle results (`cargo xtask test-corpus`), the scorecard, and the honest assessment below.

| M | Status | Where we are |
|---|---|---|
| M0 Skeleton | ✅ | workspace, gates (layers / parity / scorecard / corpus / perf / panic-hunt), CI |
| M1 Foundation | ✅ | core types, sparse model, transactional undo/redo |
| M2 Formula engine v1 | ✅ | parser, evaluator, incremental recalc, 502/514 functions; oracle 99.97% of cells (209/218 files) |
| M3 .xlsx v1 | 🟡 | values, formulas (incl. shared), styles subset, sheets, names; 259/259 corpus round-trip |
| M4 Shell v1 | ✅ | ribbon from catalog, formula bar, grid, tabs, status bar, dialogs, context menus |
| M5 Editing parity | 🟡 | audited with 50 workflow tests: cut is a move with ref fixups, tiling, Paste Special ops/transpose, AutoFill series, find/replace options, one undo step each; gaps: fill patterns like Q1..Q4, table auto-grow on paste |
| M6 Formatting parity | 🟡 | full Format Cells, number-format engine parity, borders, conditional formatting, workbook themes |
| M7 Dynamic arrays | 🟡 | spill, `#SPILL!`, FILTER/SORT/UNIQUE/SEQUENCE, LAMBDA family |
| M8 Data tools | 🟡 | sort, AutoFilter + dropdowns, validation lists, tables + dialogs, what-if (goal seek, data tables, scenarios), consolidate, forecast sheet |
| M9 Charts & objects | 🟡 | charts (mouse select/move/resize), sparklines, comment/note cards; pictures, shapes, text boxes, lines, cell checkboxes and backgrounds drawn on screen and in PDF, mouse select/move/delete |
| M10 Print & PDF | 🟡 | pagination, PDF export (standard-14 fonts), header/footer, page break preview, page layout view; no OS print dialog |
| M11 Pivot tables | 🟡 | model, layout, refresh, PivotTable Fields pane (menus, no drag-and-drop), value number formats, clickable slicers (not printed, not movable), GETPIVOTDATA/GROUPBY/PIVOTBY, .xlsx adopt-or-pass-through |
| M12 Automation | ✅ | control channel, `sp mcp` server (10 tools), macros commands; DoD met: agent workflow tasks pass in the gate |

## Honest parity assessment (2026-10-19)

Measured 2026-10-19 after wave 5: catalog wiring 806/818 (commands 304/304, functions 502/514);
oracle formula cells 99.4% match (192/218 test files fully, up from 175); 259/259 corpus files
round-trip; all perf budgets met at 20k rows (quick run).

- Feature surface that exists in some form: ~80%. Every catalog command is wired. But wave 5's
  objects, slicers and pivot field editing exist below the UI only: a user can insert a picture
  and it saves to .xlsx, yet nothing draws it. Command parity now overstates what a user sees.
- "A professional could switch for daily work": ~25%, unchanged. Lists, budgets and simple
  reports work and print; pivots are basic (no grouping/calculated fields, no field-list pane);
  regenerated pivot files have not been opened in the target app; no real users yet.
- Biggest gaps: no real-user trial yet; drawing pictures/shapes/checkboxes/slicers (screen and
  PDF); pivot UI; editing behaviour unaudited; 1M-row grid performance never measured.
- Confidence: high for formulas (oracle-backed), low for UI feel and pivots/print fidelity.

### Where we're going (priority order)

1. Formula engine correctness against the oracle corpus (it is the product).
2. .xlsx round-trip fidelity (users must not lose data).
3. Editing ergonomics: clipboard, fill, undo, keyboard navigation feel.
4. Grid performance at 1M rows.

## Current focus (wave 7)

Wave 6 (2026-10-19) landed: objects/slicers/pivot pane drawn, oracle 209/218, M5 audit, 1M-row
perf suite (`cargo xtask perf --million`). The assessment above predates it.

1. Real-user trial: hand out `cargo xtask package` builds, turn every bug into a regression test
   and a scorecard row.
2. Memory at 1M rows (7.2 GB peak): share one parse tree across filled-down formulas, evaluate
   with an offset. Also cuts cold recalc, load, save, sort and insert time.
3. Remaining oracle: XIRR/IRR iteration (13 cells), table-filter state for SUBTOTAL (7 cells).
4. Polish from wave 6: slicers in PDF and movable, italic font face, rotated hit-testing,
   drag-and-drop in the fields pane.
