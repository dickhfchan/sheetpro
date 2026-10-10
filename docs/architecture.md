# Architecture

## Layers

```text
L6  sp_ui        egui shell: ribbon (generated from catalog), formula bar, grid, tabs, status bar
L5  sp_engine    Session, CommandRegistry, *_cmds modules, control protocol (JSON in/out)
L4  sp_io        .xlsx reader/writer, .csv reader/writer
L3  sp_render    display text (format + alignment + overflow), grid geometry (render model)
L2  sp_formula   lexer → parser → AST, reference shifting, evaluator, function registry, recalc
L2  sp_ops       history, fill, sort, structural edits (insert/delete rows/cols), clipboard
L1  sp_model     Workbook / Sheet / Cell / Style / DefinedName (pure data, serde)
L0  sp_core      CellAddr, CellRange, A1 parsing, Value, ErrorKind, date serials, number-format
                 engine (shared by the grid renderer and the TEXT function), typed-input parsing
```

Rule: a crate depends only on crates in strictly lower layers (same-layer L2 crates don't
depend on each other except `sp_ops → sp_formula`, which is allowed explicitly for reference
shifting). `cargo xtask layers` parses every `Cargo.toml` and fails on violations or on any
UI crate (`egui`, `eframe`, `egui_*`, `winit`, `wgpu`) below L6.

## Data model (sp_model)

- `Workbook { sheets: Vec<Sheet>, active: usize, names: Vec<DefinedName>, styles: StyleTable,
  calc_mode, props }`
- `Sheet { name, cells: BTreeMap<u32 /*row*/, BTreeMap<u32 /*col*/, Cell>>, col_widths,
  row_heights, hidden rows/cols, merges, freeze, tab_color, show_gridlines, zoom }`
  Sparse: memory proportional to used cells; row-major ordered iteration for I/O and recalc.
- `Cell { content: Content, value: Value, style: StyleId }`
  - `Content::Value(Value)` for constants, `Content::Formula(String)` (text without `=`).
  - `value` is the cached result for formulas (and equals the constant otherwise).
- `StyleTable` interns `Style` (font, fill, alignment, border, number format) → `StyleId(u32)`;
  id 0 is the default style.

Addresses are 0-based internally (`CellAddr { row, col }`), A1 text only at the edges. Limits
match the target: 1,048,576 rows × 16,384 columns.

## Formula engine (sp_formula)

1. `lexer` → tokens (numbers, strings, refs incl. `Sheet!$A$1:B2`, `'My Sheet'!A1`, whole
   rows/cols `A:A` / `1:1`, names, functions, operators, `{array}` constants, errors).
2. `parser` (precedence climbing; Excel precedence: `:` `,`-union, space-intersection, unary
   `-`, `%`, `^`, `* /`, `+ -`, `&`, comparisons) → `Expr` AST. Nesting depth capped.
3. `shift` rewrites references for copy/paste/fill (relative parts move) and for structural
   edits (insert/delete rows/cols; refs into deleted areas become `#REF!`).
4. `eval` walks the AST against an `EvalContext` (reads cell values, resolves names) producing
   `EvalValue::{Scalar(Value), Array(Array2), Ref(SheetRange)}`; functions receive lazily
   evaluated args so `IF`/`IFERROR`/`CHOOSE` short-circuit.
5. `functions` is a `Registry` of `name → FnSpec { min_args, max_args, imp }`, one module per
   category. `fn.<NAME>` ids are what `spec/functions.toml` measures.
6. `recalc` builds the dependency graph from parsed ASTs, topologically evaluates dirty
   cells, and marks cycles `#CIRC!`-free: cells in a cycle get `0` and a circular warning
   flag (matching the target's default iterative-off behaviour). Volatile functions (NOW,
   RAND, OFFSET, INDIRECT…) are always dirty.

Dynamic-array spill is modelled in `recalc` (anchor cell owns a spill range; blocked spill =
`#SPILL!`).

## Engine (sp_engine): everything is a command

```rust
pub struct CommandSpec {
    pub id: &'static str,          // == catalog id, e.g. "home.font.bold"
    pub label: &'static str,
    pub location: &'static str,    // ribbon path, "keyboard", …
    pub shortcut: &'static str,    // Windows-style; shell maps Ctrl→Cmd on macOS
    pub params: &'static str,      // human/agent-readable params doc (JSON shape)
    pub enabled: fn(&Session) -> bool,
    pub run: fn(&mut Session, &serde_json::Value) -> Result<Outcome, CmdError>,
}
```

`Session` owns the `Workbook`, `Selection`, `History`, clipboard, file path, dirty flag,
`ViewState` (zoom, show formulas, gridlines…), and a recalc cache. Every mutation goes through
`Session::apply(Change)` so history and recalc stay consistent. The UI, the CLI, the control
channel and tests all call `Session::execute(id, params)`.

## Seam: engine → UI

`sp_ui` reads `Session` + `sp_render::GridView` (visible rows/cols, x/y offsets, display text
per cell) and emits `execute(id, params)` calls. It never mutates the workbook directly. The
ribbon is built from `spec/commands.toml` locations filtered to registered commands (missing
ones render disabled so gaps stay visible).

## Headless drivability

- `sp control` runs a Session over JSON lines on stdin/stdout (see `control-protocol.md`).
- `sp screenshot` renders the real egui shell offscreen (egui_kittest + wgpu) to PNG, after
  optionally applying a JSON-lines script of commands.
- The desktop app accepts `--control-port <port>` to expose the same protocol on localhost.
