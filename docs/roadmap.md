# Roadmap

Status legend: ✅ done · 🟡 in progress · ⬜ not started. Updated 2026-10-10.

**Parity metrics.** `cargo xtask parity` measures how much of the target's catalog (ribbon /
keyboard commands and worksheet functions) is *wired* (`parity.md`). It does **not** measure
whether those commands behave like the target or survive real files. Read it next to the
oracle results (`cargo xtask test-corpus`), the scorecard, and the honest assessment below.

| M | Status | Where we are |
|---|---|---|
| M0 Skeleton | ✅ | workspace, gates (layers / parity / scorecard / corpus / perf / panic-hunt), CI |
| M1 Foundation | ✅ | core types, sparse model, transactional undo/redo |
| M2 Formula engine v1 | ✅ | parser, evaluator, incremental recalc, 496/514 functions; oracle 93.1% of cells |
| M3 .xlsx v1 | 🟡 | values, formulas (incl. shared), styles subset, sheets, names; 259/259 corpus round-trip |
| M4 Shell v1 | ✅ | ribbon from catalog, formula bar, grid, tabs, status bar, dialogs, context menus |
| M5 Editing parity | ⬜ | clipboard w/ ref shifting, fill/autofill, insert/delete with ref fixups, find/replace |
| M6 Formatting parity | ⬜ | full Format Cells, number-format engine parity, borders, conditional formatting |
| M7 Dynamic arrays | 🟡 | spill, `#SPILL!`, FILTER/SORT/UNIQUE/SEQUENCE, LAMBDA family |
| M8 Data tools | 🟡 | sort dialog, AutoFilter, validation, remove duplicates, text to columns, tables |
| M9 Charts & objects | ⬜ | charts, sparklines, images, comments/notes |
| M10 Print & PDF | ⬜ | page setup, page breaks, PDF export |
| M11 Pivot tables | ⬜ | pivot cache, layout, refresh |
| M12 Automation | 🟡 | control channel ✅, MCP server, DoD: 10 agent workflow tasks pass |

## Honest parity assessment (2026-10-10)

Measured 2026-10-11 after wave 2: catalog wiring 733/818 (commands 237/304, functions 496/514);
oracle formula cells 93.1% match (175/218 test files fully); 259/259 corpus files round-trip;
edit latency 4–8 ms at 100k rows under machine load (budget 50 ms).

- Feature surface that exists in some form: ~45% (core grid, entry, formulas, formatting basics,
  .xlsx; nothing for charts, pivots, tables, conditional formats, validation, printing).
- "A professional could switch for daily work": ~10%. A simple budget or list works; any real
  model hits a missing function, missing dialog, or a dropped feature on save.
- Biggest gaps: charts; pivot tables; Excel tables; conditional formatting; printing/PDF;
  preserving unsupported .xlsx parts on save; no UI for filters/notes/validation dropdowns;
  structural edits don't move comments/validations/filters.
- Confidence: moderate for formulas (oracle-backed), low for UI feel (no users yet).

### Where we're going (priority order)

1. Formula engine correctness against the oracle corpus (it is the product).
2. .xlsx round-trip fidelity (users must not lose data).
3. Editing ergonomics: clipboard, fill, undo, keyboard navigation feel.
4. Grid performance at 1M rows.

## Current focus (infrastructure before the long tail)

1. Structural edits move every per-cell feature (comments, links, validations, filters, spills).
2. Preserve unknown .xlsx parts on save (charts/pivots/drawings pass through untouched).
3. Conditional formatting (engine + render + .xlsx) and Excel tables.
4. Charts (engine model + egui rendering + .xlsx).
5. Workflow tests: 10 realistic multi-command tasks over the control channel.
