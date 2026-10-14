# Roadmap

Status legend: ✅ done · 🟡 in progress · ⬜ not started. Updated 2026-10-14.

**Parity metrics.** `cargo xtask parity` measures how much of the target's catalog (ribbon /
keyboard commands and worksheet functions) is *wired* (`parity.md`). It does **not** measure
whether those commands behave like the target or survive real files. Read it next to the
oracle results (`cargo xtask test-corpus`), the scorecard, and the honest assessment below.

| M | Status | Where we are |
|---|---|---|
| M0 Skeleton | ✅ | workspace, gates (layers / parity / scorecard / corpus / perf / panic-hunt), CI |
| M1 Foundation | ✅ | core types, sparse model, transactional undo/redo |
| M2 Formula engine v1 | ✅ | parser, evaluator, incremental recalc, 502/514 functions; oracle 93.3% of cells |
| M3 .xlsx v1 | 🟡 | values, formulas (incl. shared), styles subset, sheets, names; 259/259 corpus round-trip |
| M4 Shell v1 | ✅ | ribbon from catalog, formula bar, grid, tabs, status bar, dialogs, context menus |
| M5 Editing parity | ⬜ | clipboard w/ ref shifting, fill/autofill, insert/delete with ref fixups, find/replace |
| M6 Formatting parity | 🟡 | full Format Cells, number-format engine parity, borders, conditional formatting |
| M7 Dynamic arrays | 🟡 | spill, `#SPILL!`, FILTER/SORT/UNIQUE/SEQUENCE, LAMBDA family |
| M8 Data tools | 🟡 | sort, AutoFilter + dropdowns, validation lists, tables + dialogs, what-if (goal seek, data tables, scenarios), consolidate, forecast sheet |
| M9 Charts & objects | 🟡 | charts (mouse select/move/resize), sparklines, comment/note cards; no pictures/shapes |
| M10 Print & PDF | 🟡 | pagination, PDF export (standard-14 fonts), header/footer, page break preview, page layout view; no OS print dialog |
| M11 Pivot tables | 🟡 | model, layout, refresh, slicers (model only), GETPIVOTDATA/GROUPBY/PIVOTBY, .xlsx adopt-or-pass-through |
| M12 Automation | 🟡 | control channel ✅, MCP server, DoD: 10 agent workflow tasks pass |

## Honest parity assessment (2026-10-14)

Measured 2026-10-14 after wave 4: catalog wiring 796/818 (commands 294/304, functions 502/514);
oracle formula cells 93.3% match (175/218 test files fully, unchanged since wave 3); 259/259
corpus files round-trip and pass package validation; 14 workflow tests pass; edit latency
~1.9 ms at 100k rows, PDF export of 223 pages ~190 ms (load avg ~43 during measurement).

- Feature surface that exists in some form: ~70% (grid, formulas, formatting, .xlsx, charts,
  CF, tables, pivots, what-if, printing/PDF, in-grid filter/comment/validation UI, desktop
  settings/autosave/recovery). Missing: pictures/shapes/text boxes, themes, spelling, flash fill.
- "A professional could switch for daily work": ~25%. Lists, budgets and simple reports work and
  print; pivots are basic (no grouping/calculated fields); regenerated pivot files have not been
  opened in the target app; no real users yet.
- Biggest gaps: no real-user trial yet (a trial build now exists: `cargo xtask package`);
  pictures/shapes; pivot depth and pivot UI (no field list pane); oracle stuck at 93.3%;
  formula-engine residuals (FORECAST.ETS unverified against the target).
- Confidence: moderate for formulas (oracle-backed), low for UI feel and pivots/print fidelity.

### Where we're going (priority order)

1. Formula engine correctness against the oracle corpus (it is the product).
2. .xlsx round-trip fidelity (users must not lose data).
3. Editing ergonomics: clipboard, fill, undo, keyboard navigation feel.
4. Grid performance at 1M rows.

## Current focus (infrastructure before the long tail)

1. Real-user trial: hand out `cargo xtask package` builds, turn every bug into a regression test
   and a scorecard row.
2. Oracle corpus: the 43 failing calc files (stuck at 175/218 for two waves); add oracle cases
   for pivots, FORECAST.ETS and PDF output.
3. Pivot UI: field list pane, slicer drawing, slicer/scenario .xlsx parts.
4. Long tail of catalog: pictures/shapes/text boxes, themes, spelling, flash fill, macros view.
