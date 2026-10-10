//! `sp`: SheetPro command line. Headless control server, formula eval, recalc, screenshots.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::json;
use sp_engine::Session;
use sp_engine::control::{Action, handle_line};
use std::io::{BufRead, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "sp", about = "SheetPro command line")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// JSON-lines control session on stdin/stdout (see docs/control-protocol.md).
    Control {
        #[arg(long)]
        open: Option<PathBuf>,
    },
    /// Evaluate a formula in an empty (or opened) workbook.
    Eval {
        formula: String,
        #[arg(long)]
        open: Option<PathBuf>,
        /// Print the parsed AST.
        #[arg(long)]
        trace: bool,
    },
    /// Load a workbook, recalculate, and save it.
    Recalc {
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Run one command by id with JSON params.
    Run {
        id: String,
        #[arg(default_value = "{}")]
        params: String,
        #[arg(long)]
        open: Option<PathBuf>,
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Render the real UI offscreen to a PNG.
    Screenshot {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        open: Option<PathBuf>,
        /// JSON-lines control script applied before rendering.
        #[arg(long)]
        script: Option<PathBuf>,
        #[arg(long, default_value_t = 1280.0)]
        width: f32,
        #[arg(long, default_value_t = 800.0)]
        height: f32,
    },
    /// List registered commands as JSON.
    Commands,
}

fn session_with(open: Option<&PathBuf>) -> Result<Session> {
    let mut s = Session::new();
    if let Some(p) = open {
        s.execute("file.open", &json!({ "path": p }))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok(s)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Control { open } => {
            let mut s = session_with(open.as_ref())?;
            let stdin = std::io::stdin();
            let mut out = std::io::stdout().lock();
            for line in stdin.lock().lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let (reply, action) = handle_line(&mut s, &line);
                writeln!(out, "{reply}")?;
                out.flush()?;
                match action {
                    Action::Quit => break,
                    Action::Screenshot(p) => {
                        let snap = Session::with_workbook(s.wb.clone());
                        let mut snap = snap;
                        snap.sel = s.sel;
                        snap.view = s.view.clone();
                        if let Err(e) = sp_ui::screenshot::render_png(
                            snap,
                            (1280.0, 800.0),
                            std::path::Path::new(&p),
                        ) {
                            eprintln!("screenshot failed: {e}");
                        }
                    }
                    Action::None => {}
                }
            }
        }
        Cmd::Eval {
            formula,
            open,
            trace,
        } => {
            let s = session_with(open.as_ref())?;
            let src = formula.trim_start_matches('=');
            if trace {
                match sp_formula::parse(src) {
                    Ok(e) => eprintln!("{e:#?}"),
                    Err(e) => eprintln!("{e}"),
                }
            }
            let v = sp_formula::eval_formula(&s.wb, s.sheet_index(), s.sel.active, src);
            println!("{}", v.to_display());
        }
        Cmd::Recalc { input, out } => {
            let s = session_with(Some(&input))?;
            sp_io::save(&s.wb, &out).with_context(|| format!("saving {}", out.display()))?;
            println!(
                "{} formulas recalculated → {}",
                s.last_recalc.formulas,
                out.display()
            );
        }
        Cmd::Run {
            id,
            params,
            open,
            save,
        } => {
            let mut s = session_with(open.as_ref())?;
            let p: serde_json::Value =
                serde_json::from_str(&params).context("params must be JSON")?;
            match s.execute(&id, &p) {
                Ok(r) => println!("{r}"),
                Err(e) => bail!("{e}"),
            }
            if let Some(path) = save {
                sp_io::save(&s.wb, &path)?;
            }
        }
        Cmd::Screenshot {
            out,
            open,
            script,
            width,
            height,
        } => {
            let mut s = session_with(open.as_ref())?;
            if let Some(script) = script {
                let text = std::fs::read_to_string(&script)
                    .with_context(|| format!("reading {}", script.display()))?;
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    let (reply, _) = handle_line(&mut s, line);
                    if reply.contains("\"ok\":false") {
                        eprintln!("script: {line} → {reply}");
                    }
                }
            }
            sp_ui::screenshot::render_png(s, (width, height), &out)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("{}", out.display());
        }
        Cmd::Commands => {
            println!(
                "{}",
                serde_json::to_string_pretty(sp_engine::registry().all())?
            );
        }
    }
    Ok(())
}
