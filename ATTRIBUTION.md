# Attribution

Third-party material used by SheetPro. Every row is added in the same change that brings
the material in (see AGENTS.md › Golden rules).

| What | Where | Source | License |
|---|---|---|---|
| Oracle test workbooks (not vendored; fetched by `cargo xtask corpus`) | `corpus/files/` (gitignored), pinned in `corpus/manifest.toml` | [IronCalc](https://github.com/ironcalc/IronCalc) `xlsx/tests/` @ `c88f07f0` | Apache-2.0 OR MIT |
| Rust crates | `Cargo.lock` | crates.io | see each crate |
| Native file/message dialogs (`rfd` 0.17.2, pulls in `pollster` 0.4) | `apps/sheetpro` | [crates.io/crates/rfd](https://crates.io/crates/rfd), [pollster](https://crates.io/crates/pollster) | MIT; pollster Apache-2.0 OR MIT |
| `winit` 0.30.13 (already used by eframe; direct dependency on macOS to turn off the default app menu) | `apps/sheetpro` | [crates.io/crates/winit](https://crates.io/crates/winit) | Apache-2.0 |
| App icon (grid glyph drawn in code) | `apps/sheetpro/src/icon.rs`, generated `.icns` | original SheetPro work | MIT OR Apache-2.0 |

No code, icons, templates, help text or other assets from the target application are used.
