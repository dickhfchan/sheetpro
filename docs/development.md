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
| `cargo xtask check` | Everything above that runs offline, in CI order |

## Debugging

- `SP_LOG=debug` enables tracing to stderr.
- Formula issues: `cargo run -p sp -- eval "=..." --trace` prints the AST and each step.
- UI issues: write a script, run `sp screenshot`, open the PNG.
