# Development

Toolchain: Rust (edition 2024; `rust-toolchain.toml` pins the channel). macOS, Linux and
Windows are supported targets for the desktop app.

## Build, run, test

```sh
cargo run -p sheetpro                          # desktop app
cargo run -p sheetpro -- path/to/book.xlsx     # open a file
cargo run -p sheetpro -- --control-port 7878   # plus JSON control server on 127.0.0.1:7878
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## CLI (`apps/sp`)

```sh
cargo run -p sp -- control                       # JSON-lines session on stdin/stdout
cargo run -p sp -- mcp                           # MCP server on stdio (see control-protocol.md)
cargo run -p sp -- eval "=SUM(1,2,3)"            # evaluate a formula in an empty book
cargo run -p sp -- recalc in.xlsx --out out.xlsx # load, recalc, save
cargo run -p sp -- screenshot --out shot.png [--open book.xlsx] [--script steps.jsonl]
```

## xtask

| Task | What it does |
|---|---|
| `cargo xtask parity` | Catalog vs registered commands + functions → `docs/parity.md`; asserts `>= FLOOR` |
| `cargo xtask layers` | Dependency-layer rule + no UI crates below L6 |
| `cargo xtask corpus` | Fetch pinned oracle .xlsx files into `corpus/files/` (sha256-verified) |
| `cargo xtask test-corpus` | Load each oracle, recalc, compare to cached results, round-trip; asserts floors |
| `cargo xtask panic-hunt` | Every command × adversarial params × odd states; must not panic |
| `cargo xtask perf [--quick]` | Benchmarks vs `perf/budgets.toml` |
| `cargo xtask scorecard` | Regenerates `docs/scorecard.md` (`--check` fails if stale) |
| `cargo xtask package` | Release build bundled for trial users → `dist/` (see Trial builds) |
| `cargo xtask check` | Everything above that runs offline, in CI order |

## Trial builds

```sh
cargo xtask package
```

Builds `sheetpro` in release mode and lays it out in `dist/` (gitignored):

- **macOS**: `dist/SheetPro.app` (Info.plist, `Contents/MacOS/sheetpro`, an `.icns` drawn by
  `sheetpro --write-icon` and packed with `iconutil`, README/ATTRIBUTION in `Resources`), plus
  `dist/SheetPro-<version>-macos-<arch>.zip` made with `ditto`. The bundle is ad-hoc signed
  (`codesign -s -`), not Developer ID signed or notarized: after downloading, testers
  right-click › Open (or `xattr -dr com.apple.quarantine SheetPro.app`) the first time.
- **Linux/Windows**: `dist/sheetpro-<version>-<os>-<arch>/` with the binary and docs.

What a trial user gets:

- Settings in the app data folder (macOS `~/Library/Application Support/SheetPro`, Windows
  `%APPDATA%\SheetPro`, Linux `$XDG_CONFIG_HOME/sheetpro`; override with `SHEETPRO_HOME`):
  `settings.toml` (File › Options, or `file.options`), recent files (File › Open Recent).
- Autosave every `autosave_interval_secs` (default 60) into `autosave/` on a background
  thread; never written over the user's file. A session that ends without saving is offered
  for recovery at the next launch.
- `logs/sheetpro.log` (rotated at 2 MB). Help › Report a Problem writes
  `reports/report-<time>-<n>/` (version, OS, settings without file paths, recent command
  ids, log tail, optionally the workbook); nothing is sent over the network. A panic writes
  `reports/crash-<time>-<n>/crash.txt`.
- `sheetpro --version`; `sheetpro --write-icon icon.png [--icon-size 1024]`.

`sp screenshot` uses the settings in `SHEETPRO_HOME` when it is set (gridline colour, R1C1
headings), so drawing preferences can be checked headlessly.

## Debugging

- `SP_LOG=debug` enables tracing to stderr.
- Formula issues: `cargo run -p sp -- eval "=..." --trace` prints the AST and each step.
- UI issues: write a script, run `sp screenshot`, open the PNG.
