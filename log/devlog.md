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
