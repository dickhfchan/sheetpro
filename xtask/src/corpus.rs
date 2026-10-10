//! Oracle corpus: real .xlsx files saved by the target app, whose cached formula results
//! are the expected answers.
//!
//! `fetch`: download pinned files (sha256-verified) into corpus/files/ (gitignored).
//! `test`: load each file, recalculate, compare every formula cell to its cached value,
//! round-trip through our writer, and assert the floors in scorecard/floors.toml.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sp_core::Value;
use sp_model::Content;
use std::path::PathBuf;
use std::process::Command;

#[derive(Deserialize)]
struct Manifest {
    source: Source,
    file: Vec<Entry>,
}

#[derive(Deserialize)]
struct Source {
    repo: String,
    commit: String,
    url_template: String,
}

#[derive(Deserialize, Clone)]
struct Entry {
    path: String,
    sha256: String,
    kind: String,
}

fn manifest() -> Result<Manifest> {
    let text = std::fs::read_to_string(crate::root().join("corpus/manifest.toml"))?;
    Ok(toml::from_str(&text)?)
}

fn files_dir() -> PathBuf {
    crate::root().join("corpus/files")
}

fn local(e: &Entry) -> PathBuf {
    files_dir().join(&e.path)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn present() -> bool {
    manifest()
        .map(|m| m.file.iter().all(|e| local(e).exists()))
        .unwrap_or(false)
}

pub fn fetch() -> Result<()> {
    let m = manifest()?;
    let (mut got, mut cached) = (0, 0);
    for e in &m.file {
        let dest = local(e);
        if let Ok(bytes) = std::fs::read(&dest)
            && sha256_hex(&bytes) == e.sha256
        {
            cached += 1;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let url = m
            .source
            .url_template
            .replace("{repo}", &m.source.repo)
            .replace("{commit}", &m.source.commit)
            .replace("{path}", &e.path);
        let tmp = dest.with_extension("download");
        let status = Command::new("curl")
            .args([
                "-sSfL",
                "--retry",
                "4",
                "--retry-all-errors",
                "--max-time",
                "60",
                "-o",
            ])
            .arg(&tmp)
            .arg(&url)
            .status()
            .context("running curl")?;
        if !status.success() {
            bail!("download failed: {url}");
        }
        let bytes = std::fs::read(&tmp)?;
        let h = sha256_hex(&bytes);
        if h != e.sha256 {
            let _ = std::fs::remove_file(&tmp);
            bail!(
                "sha256 mismatch for {}: expected {}, got {h}",
                e.path,
                e.sha256
            );
        }
        std::fs::rename(&tmp, &dest)?;
        got += 1;
    }
    println!(
        "corpus: {} files ({got} downloaded, {cached} cached) in corpus/files/",
        m.file.len()
    );
    Ok(())
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct FileResult {
    pub path: String,
    pub kind: String,
    pub load_error: Option<String>,
    pub compared: usize,
    pub matched: usize,
    pub skipped_volatile: usize,
    pub roundtrip_ok: bool,
    pub first_mismatches: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct OracleReport {
    pub date: String,
    pub source_commit: String,
    pub files_total: usize,
    pub calc_files_total: usize,
    pub calc_files_pass: usize,
    pub cells_compared: usize,
    pub cells_matched: usize,
    pub roundtrip_pass: usize,
    pub files: Vec<FileResult>,
}

#[derive(Deserialize, Serialize, Default)]
pub struct Floors {
    pub calc_files_pass: usize,
    pub cells_matched: usize,
    pub roundtrip_pass: usize,
}

pub fn floors() -> Floors {
    std::fs::read_to_string(crate::root().join("scorecard/floors.toml"))
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn values_match(expected: &Value, got: &Value) -> bool {
    match (expected, got) {
        (Value::Number(a), Value::Number(b)) => {
            let tol = 1e-9 * a.abs().max(1.0);
            (a - b).abs() <= tol
        }
        (Value::Text(a), Value::Text(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Error(a), Value::Error(b)) => a == b,
        // The target stores empty-string results as text; a formula yielding "" may come back
        // either way.
        (Value::Text(a), Value::Empty) | (Value::Empty, Value::Text(a)) => a.is_empty(),
        _ => false,
    }
}

fn is_volatile(src: &str) -> bool {
    sp_formula::parse(src)
        .map(|e| e.is_volatile())
        .unwrap_or(false)
}

fn check_file(path: &std::path::Path, e: &Entry) -> FileResult {
    let mut r = FileResult {
        path: e.path.clone(),
        kind: e.kind.clone(),
        ..FileResult::default()
    };
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err) => {
            r.load_error = Some(err.to_string());
            return r;
        }
    };
    let (mut wb, _) = match sp_io::xlsx_read::read_xlsx(std::io::BufReader::new(file)) {
        Ok(x) => x,
        Err(err) => {
            r.load_error = Some(err.to_string());
            return r;
        }
    };
    let expected: Vec<(usize, sp_core::CellAddr, String, Value)> = wb
        .sheets
        .iter()
        .enumerate()
        .flat_map(|(si, s)| {
            s.iter().filter_map(move |(a, c)| {
                c.formula_src()
                    .map(|f| (si, a, f.to_string(), c.value.clone()))
            })
        })
        .collect();
    let mut cache = sp_formula::AstCache::default();
    sp_formula::recalc(
        &mut wb,
        &mut cache,
        sp_formula::RecalcOptions { now: None, seed: 1 },
    );
    for (si, a, src, exp) in &expected {
        if exp.is_empty() {
            continue;
        }
        if is_volatile(src) {
            r.skipped_volatile += 1;
            continue;
        }
        r.compared += 1;
        let got = wb
            .sheets
            .get(*si)
            .map(|s| s.value(*a).clone())
            .unwrap_or_default();
        if values_match(exp, &got) {
            r.matched += 1;
        } else if r.first_mismatches.len() < 5 {
            let sheet = wb
                .sheets
                .get(*si)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            r.first_mismatches.push(format!(
                "{sheet}!{a} ={src} → expected {}, got {}",
                exp.to_display(),
                got.to_display()
            ));
        }
    }
    // Round trip: inputs and values survive our writer + reader.
    let mut buf = std::io::Cursor::new(Vec::new());
    r.roundtrip_ok = sp_io::xlsx_write::write_xlsx(&wb, &mut buf).is_ok() && {
        buf.set_position(0);
        match sp_io::xlsx_read::read_xlsx(buf) {
            Ok((back, _)) => {
                back.sheets.len() == wb.sheets.len()
                    && wb.sheets.iter().zip(&back.sheets).all(|(x, y)| {
                        x.name == y.name
                            && x.iter().all(|(a, c)| {
                                let Some(d) = y.get(a) else {
                                    return c.is_blank()
                                        || matches!(c.content, Content::Value(Value::Empty));
                                };
                                c.input_text() == d.input_text()
                                    && (c.formula_src().is_some()
                                        || values_match(&c.value, &d.value)
                                        || c.value == d.value)
                            })
                    })
            }
            Err(_) => false,
        }
    };
    r
}

pub fn test(verbose: bool) -> Result<()> {
    let m = manifest()?;
    if !present() {
        bail!("corpus not fetched: run `cargo xtask corpus`");
    }
    let mut rep = OracleReport {
        date: today(),
        source_commit: m.source.commit.clone(),
        files_total: m.file.len(),
        ..OracleReport::default()
    };
    for e in &m.file {
        let r = check_file(&local(e), e);
        if e.kind == "calc" {
            rep.calc_files_total += 1;
            if r.load_error.is_none() && r.compared > 0 && r.compared == r.matched {
                rep.calc_files_pass += 1;
            }
        }
        rep.cells_compared += r.compared;
        rep.cells_matched += r.matched;
        rep.roundtrip_pass += usize::from(r.roundtrip_ok);
        if verbose && (r.load_error.is_some() || r.compared != r.matched || !r.roundtrip_ok) {
            println!(
                "{}: {}/{} cells, roundtrip {}{}",
                e.path,
                r.matched,
                r.compared,
                r.roundtrip_ok,
                r.load_error
                    .as_ref()
                    .map(|x| format!(", load error: {x}"))
                    .unwrap_or_default()
            );
            for mm in &r.first_mismatches {
                println!("    {mm}");
            }
        }
        rep.files.push(r);
    }
    std::fs::write(
        crate::root().join("scorecard/oracle.json"),
        serde_json::to_string_pretty(&rep)?,
    )?;
    println!(
        "test-corpus: calc files {}/{} pass, cells {}/{} ({:.1}%), round-trip {}/{}",
        rep.calc_files_pass,
        rep.calc_files_total,
        rep.cells_matched,
        rep.cells_compared,
        rep.cells_matched as f64 * 100.0 / rep.cells_compared.max(1) as f64,
        rep.roundtrip_pass,
        rep.files_total
    );
    let f = floors();
    if rep.calc_files_pass < f.calc_files_pass
        || rep.cells_matched < f.cells_matched
        || rep.roundtrip_pass < f.roundtrip_pass
    {
        bail!(
            "oracle floor regressed (files {} < {} or cells {} < {} or round-trip {} < {})",
            rep.calc_files_pass,
            f.calc_files_pass,
            rep.cells_matched,
            f.cells_matched,
            rep.roundtrip_pass,
            f.roundtrip_pass
        );
    }
    Ok(())
}

pub fn today() -> String {
    let (y, m, d) =
        sp_core::date::serial_to_date(sp_core::date::now_serial().floor()).unwrap_or((1970, 1, 1));
    format!("{y:04}-{m:02}-{d:02}")
}
