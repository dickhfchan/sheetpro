# Dev log

Terse entries, newest last: what landed, numbers, still open.

## 2026-10-10 — M0 skeleton (one-shot)

Landed:
- Spec: `spec/commands.toml` (304 ribbon/keyboard commands), `spec/functions.toml` (514 functions).
- Oracle corpus: 259 IronCalc test workbooks pinned by sha256 (`corpus/manifest.toml`).
- Workspace: sp_core (A1, values, dates, number formats, typed-input parsing) → sp_model →
  sp_formula (parser, printer, ref shifting, evaluator, 254 functions, worklist recalc) /
  sp_ops (transactions, undo/redo, fill, sort, insert/delete with fixups, clipboard) →
  sp_render → sp_io (.xlsx read/write, CSV) → sp_engine (Session, 184 commands, control
  protocol) → sp_ui (egui shell, offscreen screenshots) → apps (sheetpro, sp).
- Gates: `cargo xtask parity | layers | corpus | test-corpus | perf | scorecard | panic-hunt | check`.
- Clippy denies unwrap/expect/panic/indexing outside tests; panic_hunt fuzzes every command.

Numbers:
- Parity: commands 184/304, functions 254/514 (floors set to these).
- Oracles: 64/218 formula files fully match; 60,600/73,244 formula cells (82.7%); round-trip 259/259.
- Perf @100k rows: recalc cold 197 ms, warm 110 ms; xlsx load 278 ms, save 321 ms; sort 596 ms;
  edit one cell 110 ms (**over 50 ms budget**: full recalc per edit).

Still open:
- Incremental recalc (dirty tracking + dependents graph) to bring `edit_one_cell` under budget.
- 424 oracle cells fail with #NAME? (unimplemented functions); date-function edge cases
  (DATEVALUE/EDATE/EOMONTH/WORKDAY validation); dynamic-array spill.
- Grid doesn't draw borders, wrap, indent, rotation, vertical alignment (see scorecard › dead settings).
- No dialogs; ribbon commands that need params run with defaults or report an error.
- Cut/paste doesn't re-point references to moved cells.

## 2026-10-10 — Wave 1 (4 parallel arcs)

Landed:
- A recalc: dependency graph (`sp_formula::deps`) + `recalc_dirty`; engine recalcs only changed
  cells and dependents. Fixed two order-dependent evaluator bugs (pending cell mistaken for a
  cycle; formula returning a reference not waiting on its target).
- B dates: DATEVALUE/TIMEVALUE parsing, EDATE/EOMONTH/DAYS/DAYS360/NETWORKDAYS/WORKDAY
  validation; 9 DATE_AND_TIME oracle files now pass.
- C functions: +143 (Statistical distributions/regression, all Engineering incl. IM*/Bessel/CONVERT,
  all Compatibility aliases); special functions in `functions/special.rs`.
- D grid: borders (6 styles, shared edges once), wrap, vertical alignment, indent, rotation, font
  family, merges framed; sample in `docs/img/formatting.png`.

Numbers (before → after):
- Functions 254 → 397/514; commands 184/304 (unchanged). Floors raised.
- Oracles: files 64 → 114/218; cells 60,600 → 65,468/73,244 (82.7% → 89.4%); round-trip 259/259.
- Perf @100k: edit_one_cell 110 → 1.7 ms (now within budget); others unchanged within budget.
- Dead settings: only Style.locked / Style.hidden_formula remain (sheet protection not built).

Process note: SheetPro is a git repo nested inside ~/projects (another repo), so the Agent tool's
`isolation: "worktree"` created worktrees of the *parent* repo. Create worktrees manually:
`git worktree add ../sheetpro-waveN/<arc> -b waveN/<arc>` and copy `corpus/files` in.

Still open:
- Implicit intersection / `_xlfn.SINGLE` and dynamic-array spill (~15 date cells, more elsewhere).
- COUNTIF/COUNTIFS criteria mismatches; FORECAST.ETS family; LINEST collinear handling.
- BESSELJ/Y order 0 differ ~1e-9 from the target's approximation (50 cells).
- Incremental recalc falls back to full for circular workbooks, sheet-level edits, undo/redo.
- Borders commands can't set a colour; only built-in fonts (Arial/Times fall back).
- Commands 120 missing; no dialogs.

## 2026-10-11 — Wave 2 (4 parallel arcs)

Landed:
- 1 arrays: dynamic-array spill (`Sheet::spills`, spilled values in `Cell.value` with empty
  content), #SPILL!, `A1#`, `@`/`_xlfn.SINGLE` (`Expr::Implicit`), legacy-formula `@` rewrite on
  load, dynamic-array save (`cm` + metadata.xml); 22 array functions; LAMBDA family incl. direct
  invocation and lambdas via LET/defined names.
- 2 funcs: +74 functions (bonds/coupons/odd periods, XIRR/XNPV, Database, regex engine (own,
  step-bounded), TEXTSPLIT, *B text, AGGREGATE, CELL/INFO); criteria engine rewritten in criteria.rs.
- 3 cmds: +51 commands (AutoFilter, advanced filter, outline/subtotal, comments/notes, hyperlinks,
  protection enforced in `Session::transact`, data validation on entry, page layout, auditing, fill
  series, Go To Special, paste link); `sp_model::features`, `sp_io::xlsx_extras` round-trip.
- 4 dialogs: Format Cells (6 tabs), Find/Replace, Go To, Sort, Paste Special, Insert Function,
  Name Manager, small prompts, JSON params fallback form, ribbon combos/palettes, context menus;
  egui_kittest UI tests; `sp screenshot --dialog`.

Integration: conflicts in sp_model/lib.rs, sp_io/xlsx_read.rs (arrays vs cmds) and
functions/mod.rs (arrays vs funcs) were additive; kept both. Overlapping implementations
(MDETERM/MINVERSE/MUNIT/ARRAYTOTEXT/VALUETOTEXT/TEXTSPLIT): arrays registers last (oracle identical either order).

Numbers (before → after):
- Commands 184 → 237/304; functions 397 → 496/514. Floors raised.
- Oracles: files 114 → 175/218; cells 65,468 → 68,176/73,244 (89.4% → 93.1%); round-trip 259/259.
- Perf: machine load avg ~18 during measurement (non-cargo). Wave-1 commit measured under the same
  load: within noise everywhere except **xlsx_load 539 → 740 ms (+37%, real)** from extras/rich-value
  parsing and the legacy `@` rewrite. All benchmarks still within budget.

Still open:
- xlsx_load regression; re-baseline perf on an idle machine.
- Row/col insert/delete doesn't shift comments, hyperlinks, validations, filter range, page breaks,
  outline levels, or fixed (legacy) array areas.
- SUBTOTAL ignores neither nested SUBTOTAL nor filtered/hidden rows.
- Remove superseded COUNTIF/SUMIF-family code in stats.rs and `math::criteria_mask`.
- eval.rs: numeric-text coercion (`"--8"`, currency symbols), negative base ^ fractional exponent,
  element-wise lifting for DAY/MONTH/YEAR/POWER/EXACT/MID; INDIRECT R1C1; SEQUENCE(-1).
- Hidden formulas still shown by formula bar / control `get`; no UI for arrows, circles, notes,
  filter dropdowns.
- Financial residuals (ACCRINT 30/360, ODDF/ODDLPRICE day counts); BESSELJ/Y precision.
- Missing commands (67): charts, pivot tables, tables, conditional formatting, print/PDF, windows.

## 2026-10-12 — Wave 3 (4 parallel arcs)

Landed:
- 1 struct: insert/delete rows/cols move comments, links, validations, AutoFilter, page breaks,
  outline levels, spills (`features::LineShift`); SUBTOTAL skips nested SUBTOTAL/AGGREGATE and
  filtered (1–11) / hidden (101–111) rows; removed superseded criteria code;
  `sp_engine/tests/workflows.rs` (11 control-channel workflows) — found and fixed rename/insert not
  updating validation sources and hyperlink targets.
- 2 io: pass-through of unmodelled parts/elements (`sp_model::passthrough`, `sp_io::xlsx_passthrough`,
  HANDLED_* registry), `validate_package` + `tests/package_validity.rs` (259/259 valid, also after
  duplicate/rename/reorder/delete); xlsx_load regression fixed (split extras at opening sheetData,
  fast path for canonical formulas, fewer allocations).
- 3 cftables: conditional formatting (all rule kinds, data bars, colour scales, vector icon sets),
  Excel tables, structured references (`Expr::Table`, `structref.rs`), auto-expand, CF/table
  shifting on insert/delete (`sp_ops/table_shift.rs`); `[@Col]` saved qualified.
- 4 charts: 9 chart commands + 3 sparkline commands, `sp_render/chart.rs` geometry, `sp_ui/chart.rs`,
  DrawingML read/write; charts in drawings we fully model are adopted, others stay passed through;
  anchors/series shift on insert/delete (`sp_ops/chart_shift.rs`).

Integration: cftables and charts conflicted with io's writer refactor (RawRel ids); each arc agent
merged main into its own branch and adapted (sequentially), then merged cleanly.

Numbers (before → after):
- Commands 237 → 259/304; functions 496/514. Floors raised.
- Oracles: files 175/218 (unchanged); cells 68,176 → 68,359 (93.1% → 93.3%); round-trip 259/259;
  package validity 259/259 (new gate).
- Perf (load avg 19–30, non-cargo): xlsx_load 740 → ~305 ms (regression fixed); others in budget.

Still open:
- package_validity no longer checks passed-through drawing/chart parts byte-for-byte (adopted chart
  parts are regenerated) — narrow the exemption to adopted charts only.
- Charts in drawings with pictures/shapes/unsupported types are kept but not shown; shared captured
  drawings drop new charts; no mouse select/move of charts.
- Sort doesn't move comments/links/validations; column rename doesn't rewrite structured refs;
  filter/sort don't act on tables; no CF/table dialogs.
- Captured parts (chart series, pivot sources) aren't rewritten on sheet rename/delete.
- Ribbon groups overflow/overlap text when a tab has many commands (seen on Home in charts.png).
- Missing commands (45): pivot tables, slicers, pictures/shapes, print/PDF, windows/split, themes.
- Re-baseline perf on an idle machine.
