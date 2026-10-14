# Attribution

Third-party material used by SheetPro. Every row is added in the same change that brings
the material in (see AGENTS.md › Golden rules).

| What | Where | Source | License |
|---|---|---|---|
| Oracle test workbooks (not vendored; fetched by `cargo xtask corpus`) | `corpus/files/` (gitignored), pinned in `corpus/manifest.toml` | [IronCalc](https://github.com/ironcalc/IronCalc) `xlsx/tests/` @ `c88f07f0` | Apache-2.0 OR MIT |
| Standard-14 font advance widths (Helvetica, Times, Courier families), extracted and re-ordered to WinAnsi | `crates/sp_render/src/std14.rs`, license beside it in `crates/sp_render/src/std14_LICENSE.txt` | Adobe Core 14 AFM files (Adobe Systems, 1985–1997) | Adobe AFM license (use, copy, distribute, modify for any purpose with notices retained and modifications noted) |
| Rust crates | `Cargo.lock` | crates.io | see each crate |

No code, icons, templates, help text or other assets from the target application are used.
