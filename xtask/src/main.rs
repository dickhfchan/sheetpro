//! Project gates. `cargo xtask <task>`; see docs/development.md.

mod corpus;
mod layers;
mod parity;
mod perf;
mod scorecard;

use anyhow::{Result, bail};
use std::path::PathBuf;
use std::process::Command;

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn cargo(args: &[&str]) -> Result<()> {
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(args)
        .current_dir(root())
        .status()?;
    if !status.success() {
        bail!("cargo {} failed", args.join(" "));
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    match args.first().map(String::as_str) {
        Some("parity") => parity::run(flag("--check")),
        Some("layers") => layers::run(),
        Some("corpus") => corpus::fetch(),
        Some("test-corpus") => corpus::test(flag("--verbose")),
        Some("perf") => perf::run(flag("--quick")),
        Some("scorecard") => scorecard::run(flag("--check")),
        Some("panic-hunt") => cargo(&[
            "test",
            "--release",
            "-q",
            "-p",
            "sp_engine",
            "--test",
            "panic_hunt",
        ]),
        Some("check") => {
            cargo(&["build", "--workspace", "-q"])?;
            cargo(&[
                "clippy",
                "--workspace",
                "--all-targets",
                "-q",
                "--",
                "-D",
                "warnings",
            ])?;
            cargo(&["test", "--workspace", "-q", "--exclude", "sp_engine"])?;
            cargo(&["test", "--release", "-q", "-p", "sp_engine"])?;
            layers::run()?;
            parity::run(true)?;
            if corpus::present() {
                corpus::test(false)?;
            } else {
                println!("corpus: not fetched (run `cargo xtask corpus`); skipping oracle gate");
            }
            scorecard::run(true)?;
            println!("check: all gates green");
            Ok(())
        }
        _ => {
            eprintln!(
                "usage: cargo xtask <parity [--check] | layers | corpus | test-corpus [--verbose] | perf [--quick] | scorecard [--check] | panic-hunt | check>"
            );
            bail!("unknown task")
        }
    }
}
