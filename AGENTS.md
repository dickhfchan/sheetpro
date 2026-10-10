# AGENTS.md: guide for AI agents and contributors

SheetPro is an open-source, clean-room reimplementation of a desktop spreadsheet application
(feature target: Microsoft Excel), written in Rust with an egui shell. The goal is 1:1 parity
(same ribbon commands, shortcuts, worksheet functions, calculation results and .xlsx fidelity)
with better performance, and every feature drivable by agents. Read this file first, then `docs/`.

SheetPro is not affiliated with or endorsed by Microsoft. "Excel" is a trademark of Microsoft;
it may appear only in descriptive text ("a reimplementation of Excel"), never as branding.

## 1. Orientation

| Read | Why |
|---|---|
| `docs/architecture.md` | Crate map, dependency layers, engine/UI seam, data model |
| `docs/development.md` | Build, test, run, drive the app programmatically |
| `docs/control-protocol.md` | JSON-lines control channel: drive and screenshot the app headlessly |
| `docs/roadmap.md` | Milestones, **Honest parity assessment**, **Current focus** |
| `docs/parity.md` | Generated: every catalog item (commands + functions), live or missing |
| `docs/scorecard.md` | Generated: perf budgets, oracle floors, per-area checklists, dead settings |
| `spec/commands.toml`, `spec/functions.toml` | The catalog: the measurable spec |

## 2. Workspace map

```text
crates/sp_core     L0 foundation: addresses, A1 refs, Value, errors, date serials (pure)
crates/sp_model    L1 data model: Workbook, Sheet, Cell, Style, names (pure data, serde)
crates/sp_formula  L2 formula lexer/parser/AST, ref adjustment, evaluator, functions, recalc
crates/sp_ops      L2 operations: history (undo/redo), fill, sort, insert/delete, clipboard
crates/sp_render   L3 number formats, display text, grid layout (render model, no UI libs)
crates/sp_io       L4 import/export: .xlsx (zip + quick-xml), .csv
crates/sp_engine   L5 Session + command registry (every action is a command), control protocol
crates/sp_ui       L6 egui shell (thin, data-driven), offscreen screenshots
apps/sheetpro      desktop entry point (eframe)
apps/sp            CLI: run commands, headless control server, screenshot
xtask/             parity | layers | scorecard | corpus | test-corpus | perf | panic-hunt | check
```

A crate may depend only on lower layers (enforced by `cargo xtask layers`). `egui`/`eframe`
never appear below L6. Register any new crate in `xtask/src/layers.rs`.

## 3. Golden rules

0. **Never crash.** Non-test code returns errors, never panics, for any input, params, or state.
   `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!` and unchecked slice indexing are denied
   by clippy outside tests. Input-derived numbers are hostile: bounds-check, checked arithmetic,
   cap allocations (ranges are capped by `sp_core::MAX_CELLS_PER_OP`), bound recursion (formula
   nesting is capped). Every crash fix ships with a regression test that crashed before.
1. **Everything is a command.** New user-visible behaviour = a `CommandSpec` in `sp_engine`
   with id, label, location, shortcut, params doc, `enabled`, `run`, plus tests. Use the
   **exact id from `spec/commands.toml`** and the item goes live automatically. Worksheet
   functions go in `sp_formula::functions::<category>` and register by name (`fn.<NAME>`).
   Only pure view state (scroll, panel open/closed) belongs to the shell.
2. **Clean-room.** Study the target for behaviour and look only. Never copy its code, assets,
   icons, help text or templates. Function names and file-format element names are
   interoperability facts; implement behaviour from public references and observed results.
   Third-party assets: permissive license, license file beside it, row in `ATTRIBUTION.md`
   in the same change.
3. **Tests are the gate.** Every change has tests. Never regress the parity `FLOOR` or the
   oracle floor (`scorecard/floors.toml`).
4. **The UI is thin and data-driven.** The ribbon is generated from the catalog + registry.
   UI state is serializable; colours come from `sp_ui::theme`, not literals in widgets.
5. **Verify UI changes visually.** `cargo run -p sp -- screenshot --out shot.png` (optionally
   with `--script steps.jsonl`) and look at the PNG.
6. **Performance is a feature.** Benchmark heavy ops at realistic sizes (`cargo xtask perf`);
   record before/after in the devlog.

## 4. Picking work

Infrastructure first, then low-hanging parity, then the long tail.

0. `docs/roadmap.md` › Honest parity assessment.
1. `docs/scorecard.md`: failing oracle files, over-budget perf, missing/partial rows, dead settings.
2. `docs/roadmap.md` › Current focus.
3. `cargo xtask parity` → `docs/parity.md`: missing items. Cheapest: a missing command or
   function whose algorithm already exists (e.g. `data.sort_az` when `home.editing.sort_az` is live).
4. `log/devlog.md`: "Still open" bullets of recent entries.

When parity rises, raise `FLOOR` in `xtask/src/parity.rs` (never lower it).

## 5. Before you finish a task

```sh
cargo test -p <touched crates>
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask layers
cargo xtask parity        # if you added commands/functions; commit regenerated docs/parity.md
cargo xtask panic-hunt    # if you added/changed commands
cargo xtask test-corpus   # if you touched formulas, functions, or import/export
cargo xtask scorecard     # if you moved a number; commit regenerated docs/scorecard.md
cargo xtask perf --quick  # if you touched a hot path (recalc, xlsx, grid layout)
```

Or all of it: `cargo xtask check`. Then append a terse entry to `log/devlog.md` (what landed,
numbers, still open). Sessions can end abruptly; the dev log plus a green tree is how the next
agent picks up.

## 6. Parallel agents

- Use your own build dir: `CARGO_TARGET_DIR=target/agent-<name>`. Edit only the files you own.
- New commands go in a **new module** `crates/sp_engine/src/commands/<area>_cmds.rs` with a
  `pub fn specs() -> Vec<CommandSpec>`; add one line to `commands/mod.rs`. New functions go in
  `crates/sp_formula/src/functions/<category>.rs` with `pub fn register(r: &mut Registry)`.
- Shared registries get small surgical edits; re-read before editing.
- If someone else's in-progress edit breaks the build, wait and retry; don't fix their files.
- Keep every manifest valid at all times: write to a temp file elsewhere, then `mv` into place.
- Delete your build dir when finished.
