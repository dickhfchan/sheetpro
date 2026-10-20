//! `cargo xtask smoke [--bin path] [--full] [--only area]`: test a built desktop app end to
//! end over its control port.
//!
//! - Default (run by `cargo xtask package`): scenario areas in `scenarios.rs`, from launch
//!   to clean exit: formulas, editing, formatting, data tools, sheets and names, objects,
//!   pivots, files, print/PDF, hostile input, real-window rendering.
//! - `--full` adds every registered command against a populated workbook (`sweep.rs`), every
//!   oracle corpus file opened, saved and reopened (`corpus.rs`), and 100k-row timings
//!   (`perf.rs`).
//!
//! Each area launches its own app, with `SHEETPRO_HOME` in `target/smoke/<area>/home` so the
//! user's settings and recent files are never touched. Any crash report the app writes there
//! fails the run. Set `SP_SKIP_SMOKE=1` to skip it in `package` (no display).

mod corpus;
mod perf;
mod scenarios;
mod sweep;

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Opts {
    pub full: bool,
    pub only: Option<String>,
}

pub struct Ctx {
    pub bin: PathBuf,
    /// Scratch directory for this run (`target/smoke`).
    pub wd: PathBuf,
    area: String,
    passed: usize,
    known: usize,
    failed: Vec<String>,
}

impl Ctx {
    pub fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
        let name = format!("{}: {name}", self.area);
        if ok {
            self.passed += 1;
            println!("PASS {name}");
        } else {
            let mut d = format!("{detail:?}");
            if d.len() > 400 {
                let cut = (0..=400)
                    .rev()
                    .find(|i| d.is_char_boundary(*i))
                    .unwrap_or(0);
                d.truncate(cut);
                d.push('…');
            }
            println!("FAIL {name}  [{d}]");
            self.failed.push(name);
        }
    }

    /// A known difference from the target, tracked without failing the run. If it starts
    /// passing, that fails: remove it from the known list so it is guarded from then on.
    /// (No known issues are tracked right now; kept for the next one.)
    #[allow(dead_code)]
    pub fn known(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug, why: &str) {
        let full = format!("{}: {name}", self.area);
        if ok {
            println!("FAIL {full}  [known issue now passes: make it a normal check]");
            self.failed.push(full);
        } else {
            println!("KNOWN {full}  [{detail:?}] ({why})");
            self.known += 1;
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.wd.join(&self.area).join(name)
    }

    pub fn launch(&mut self) -> Result<App> {
        App::launch(&self.bin, &self.wd, &self.wd.join(&self.area).join("home"))
    }
}

pub struct App {
    child: Child,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

impl App {
    fn launch(bin: &Path, wd: &Path, home: &Path) -> Result<App> {
        let port = free_port()?;
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(wd.join("app.log"))?;
        let child = Command::new(bin)
            .args(["--control-port", &port.to_string()])
            .env("SHEETPRO_HOME", home)
            .stdout(Stdio::from(out.try_clone()?))
            .stderr(Stdio::from(out))
            .spawn()
            .with_context(|| format!("launching {}", bin.display()))?;
        let start = Instant::now();
        let stream = loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => break s,
                Err(_) if start.elapsed() < Duration::from_secs(30) => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => bail!("the app did not open its control port: {e}"),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        Ok(App {
            child,
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        })
    }

    pub fn raw(&mut self, msg: Json) -> Result<Json> {
        writeln!(self.writer, "{msg}")?;
        self.writer.flush()?;
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("the app closed the connection");
        }
        Ok(serde_json::from_str(&line)?)
    }

    /// Run a command; `Err` carries the app's error message (or a transport error).
    pub fn try_ex(&mut self, id: &str, params: Json) -> Result<Json, String> {
        match self.raw(json!({"op": "exec", "id": id, "params": params})) {
            Ok(r) if r["ok"] == json!(true) => Ok(r["result"].clone()),
            Ok(r) => Err(r["error"].as_str().unwrap_or("error").to_string()),
            Err(e) => Err(format!("transport: {e}")),
        }
    }

    /// Run a command that should succeed; a failure is recorded and yields `Null`.
    pub fn ex(&mut self, c: &mut Ctx, id: &str, params: Json) -> Json {
        match self.try_ex(id, params) {
            Ok(v) => v,
            Err(e) => {
                c.check(&format!("command {id} runs"), false, e);
                Json::Null
            }
        }
    }

    pub fn set(&mut self, c: &mut Ctx, cell: &str, input: &str) {
        self.ex(c, "cell.set", json!({"cell": cell, "input": input}));
    }

    pub fn get(&mut self, range: &str) -> Json {
        self.raw(json!({"op": "get", "range": range}))
            .map(|r| r["result"].clone())
            .unwrap_or(Json::Null)
    }

    pub fn cell(&mut self, cell: &str, key: &str) -> Json {
        self.get(cell)[0][0][key].clone()
    }

    pub fn val(&mut self, cell: &str) -> Json {
        self.cell(cell, "value")
    }

    pub fn state(&mut self) -> Json {
        self.raw(json!({"op": "state"}))
            .map(|r| r["result"].clone())
            .unwrap_or(Json::Null)
    }

    /// The app still answers and has not exited.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None)) && self.state()["sheets"].is_array()
    }

    /// Capture the real window; waits for the file (written after the next frame).
    pub fn screenshot(&mut self, path: &Path) -> bool {
        let _ = std::fs::remove_file(path);
        if self
            .raw(json!({"op": "screenshot", "out": path}))
            .map_or(true, |r| r["ok"] != json!(true))
        {
            return false;
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if std::fs::metadata(path).is_ok_and(|m| m.len() > 0) {
                // Give the writer a moment to finish.
                std::thread::sleep(Duration::from_millis(200));
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    pub fn quit(mut self, c: &mut Ctx, name: &str) {
        let _ = self.raw(json!({"op": "quit"}));
        let start = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(st)) => {
                    c.check(name, st.success(), st);
                    return;
                }
                Ok(None) if start.elapsed() < Duration::from_secs(15) => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => {
                    let _ = self.child.kill();
                    c.check(name, false, "had to kill it");
                    return;
                }
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // An area that bailed early must not leave an app window behind.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn num(v: &Json) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

pub fn close(v: &Json, want: f64) -> bool {
    (num(v) - want).abs() < 1e-9 * want.abs().max(1.0)
}

pub fn object_count(v: &Json) -> usize {
    v["objects"].as_array().map_or(0, Vec::len)
}

/// Every cell value of a `get` grid, row by row.
pub fn values(grid: &Json) -> Vec<Json> {
    grid.as_array()
        .into_iter()
        .flatten()
        .filter_map(Json::as_array)
        .flatten()
        .map(|c| c["value"].clone())
        .collect()
}

pub fn is_error(v: &Json, code: &str) -> bool {
    v["error"] == json!(code)
}

/// The bundled app's binary under `dist/` (what `cargo xtask package` produced).
pub fn packaged_binary(root: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.join("dist/SheetPro.app/Contents/MacOS/sheetpro")
    } else {
        let name = if cfg!(windows) {
            "sheetpro.exe"
        } else {
            "sheetpro"
        };
        let dir = format!(
            "sheetpro-{}-{}-{}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        root.join("dist").join(dir).join(name)
    }
}

type Area = fn(&mut Ctx) -> Result<()>;

fn areas(full: bool) -> Vec<(&'static str, Area)> {
    let mut v: Vec<(&'static str, Area)> = vec![
        ("launch", scenarios::launch),
        ("formulas", scenarios::formulas),
        ("workflow", scenarios::workflow),
        ("editing", scenarios::editing),
        ("formatting", scenarios::formatting),
        ("data", scenarios::data_tools),
        ("sheets", scenarios::sheets_and_names),
        ("files", scenarios::files),
        ("print", scenarios::print),
        ("hostile", scenarios::hostile),
        ("render", scenarios::render),
    ];
    if full {
        v.push(("sweep", sweep::run));
        v.push(("corpus", corpus::run));
        v.push(("perf", perf::run));
    }
    v
}

pub fn run(bin: &Path, opts: &Opts) -> Result<()> {
    if !bin.exists() {
        bail!(
            "no app binary at {} (run `cargo xtask package`)",
            bin.display()
        );
    }
    let wd = crate::root().join("target/smoke");
    let _ = std::fs::remove_dir_all(&wd);
    std::fs::create_dir_all(&wd)?;
    let mut c = Ctx {
        bin: bin.to_path_buf(),
        wd: wd.clone(),
        area: String::new(),
        passed: 0,
        known: 0,
        failed: Vec::new(),
    };
    let all = areas(true);
    let run: Vec<(&str, Area)> = match &opts.only {
        Some(o) => {
            let picked: Vec<_> = all.into_iter().filter(|(n, _)| n == o).collect();
            if picked.is_empty() {
                let names: Vec<&str> = areas(true).iter().map(|(n, _)| *n).collect();
                bail!("no smoke area '{o}' (areas: {})", names.join(", "));
            }
            picked
        }
        None => areas(opts.full),
    };
    let started = Instant::now();
    for (name, f) in run {
        c.area = name.to_string();
        std::fs::create_dir_all(wd.join(name))?;
        let t = Instant::now();
        if let Err(e) = f(&mut c) {
            c.check("area ran to the end", false, format!("{e:#}"));
        }
        println!("-- {name}: {:.1}s", t.elapsed().as_secs_f64());
    }

    c.area = "app".to_string();
    let log = std::fs::read_to_string(wd.join("app.log")).unwrap_or_default();
    let bad: Vec<&str> = log
        .lines()
        .filter(|l| {
            let l = l.to_lowercase();
            l.contains("panic") || l.contains("fatal")
        })
        .take(5)
        .collect();
    c.check("no panics in the app's output", bad.is_empty(), &bad);
    // Every area's settings folder (`<area>/home/reports/crash-*`).
    let crashes: Vec<PathBuf> = std::fs::read_dir(&wd)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|area| std::fs::read_dir(area.path().join("home/reports")).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("crash"))
        })
        .collect();
    c.check("no crash reports written", crashes.is_empty(), &crashes);

    println!(
        "smoke: {}/{} checks passed, {} known issues, in {:.0}s (outputs in {})",
        c.passed,
        c.passed + c.failed.len(),
        c.known,
        started.elapsed().as_secs_f64(),
        wd.display()
    );
    if !c.failed.is_empty() {
        for f in &c.failed {
            println!("  failed: {f}");
        }
        bail!("smoke test failed ({} checks)", c.failed.len());
    }
    Ok(())
}
