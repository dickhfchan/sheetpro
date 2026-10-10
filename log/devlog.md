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
