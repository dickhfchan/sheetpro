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
| M2 Formula engine v1 | 🟡 | parser, evaluator, incremental recalc, 397/514 functions; no spill / LAMBDA |
| M3 .xlsx v1 | 🟡 | values, formulas (incl. shared), styles subset, sheets, names; 259/259 corpus round-trip |
| M4 Shell v1 | 🟡 | ribbon from catalog, formula bar, grid, sheet tabs, status bar, screenshots; no dialogs |
| M5 Editing parity | ⬜ | clipboard w/ ref shifting, fill/autofill, insert/delete with ref fixups, find/replace |
| M6 Formatting parity | ⬜ | full Format Cells, number-format engine parity, borders, conditional formatting |
| M7 Dynamic arrays | ⬜ | spill, `#SPILL!`, FILTER/SORT/UNIQUE/SEQUENCE, LAMBDA family |
| M8 Data tools | ⬜ | sort dialog, AutoFilter, validation, remove duplicates, text to columns, tables |
| M9 Charts & objects | ⬜ | charts, sparklines, images, comments/notes |
| M10 Print & PDF | ⬜ | page setup, page breaks, PDF export |
| M11 Pivot tables | ⬜ | pivot cache, layout, refresh |
| M12 Automation | 🟡 | control channel ✅, MCP server, DoD: 10 agent workflow tasks pass |

## Honest parity assessment (2026-10-10)

Measured 2026-10-10 after wave 1: catalog wiring 581/818 (commands 184/304, functions 397/514);
oracle formula cells 89.4% match (114/218 test files fully); 259/259 corpus files round-trip;
edit latency 1.7 ms at 100k rows (budget 50 ms).

- Feature surface that exists in some form: ~30% (core grid, entry, formulas, formatting basics,
  .xlsx; nothing for charts, pivots, tables, conditional formats, validation, printing).
- "A professional could switch for daily work": ~5%. A simple budget or list works; any real
  model hits a missing function, missing dialog, or a dropped feature on save.
- Biggest gaps: dynamic arrays + LAMBDA family; dialogs (Format Cells, Find, Sort, Paste
  Special); charts; tables; conditional formatting; incremental recalc;
  preserving unsupported .xlsx parts on save.
- Confidence: moderate for formulas (oracle-backed), low for UI feel (no users yet).

### Where we're going (priority order)

1. Formula engine correctness against the oracle corpus (it is the product).
2. .xlsx round-trip fidelity (users must not lose data).
3. Editing ergonomics: clipboard, fill, undo, keyboard navigation feel.
4. Grid performance at 1M rows.

## Current focus (infrastructure before the long tail)

1. Implicit intersection / `@` / `_xlfn.SINGLE`, then dynamic arrays (spill + #SPILL!) and
   FILTER/SORT/UNIQUE/LAMBDA family.
2. Function long tail: Financial (bonds/coupons), Text, Lookup, Database, Math remaining.
3. Criteria engine parity (COUNTIF/SUMIF family mismatches in the corpus).
4. Missing commands (120): dialogs (Format Cells, Find/Replace, Sort, Paste Special), AutoFilter.
