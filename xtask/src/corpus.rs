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
    /// Every mismatch (only collected with `--dump`; not serialized).
    #[serde(skip)]
    pub all_mismatches: Vec<Mismatch>,
}

/// One failing cell, for `test-corpus --dump`.
#[derive(Default, Clone)]
pub struct Mismatch {
    pub sheet: String,
    pub cell: String,
    pub formula: String,
    pub expected: String,
    pub got: String,
}

/// Options for `cargo xtask test-corpus`.
#[derive(Default)]
pub struct TestOpts {
    pub verbose: bool,
    /// Write every mismatching cell to this TSV path, plus a grouped summary on stdout.
    pub dump: Option<PathBuf>,
    /// Only check files whose path contains this substring (floors are not checked).
    pub only: Option<String>,
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
        // Some corpus files were saved by a writer that caches error results as strings
        // (`t="str"` with `<v>#NUM!</v>`); the target itself caches them as errors.
        (Value::Text(a), Value::Error(b)) => &**a == b.as_str(),
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

/// The moment the file was last calculated, read from a cached `NOW()` (or `TODAY()`)
/// result, so formulas that depend on today's date recalculate as they did when saved.
fn saved_now(cells: &[(usize, sp_core::CellAddr, String, Value)]) -> Option<f64> {
    let cached = |name: &str| {
        cells.iter().find_map(|(_, _, src, v)| match v {
            Value::Number(n) if src.trim().eq_ignore_ascii_case(name) => Some(*n),
            _ => None,
        })
    };
    cached("NOW()").or_else(|| cached("TODAY()"))
}

fn check_file(path: &std::path::Path, e: &Entry, dump: bool) -> FileResult {
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
        sp_formula::RecalcOptions {
            now: saved_now(&expected),
            seed: 1,
        },
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
        } else {
            let sheet = wb
                .sheets
                .get(*si)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            if r.first_mismatches.len() < 5 {
                r.first_mismatches.push(format!(
                    "{sheet}!{a} ={src} → expected {}, got {}",
                    exp.to_display(),
                    got.to_display()
                ));
            }
            if dump {
                r.all_mismatches.push(Mismatch {
                    sheet,
                    cell: a.to_string(),
                    formula: src.clone(),
                    expected: exp.to_display(),
                    got: got.to_display(),
                });
            }
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

/// Outermost function name of a formula (or `(expr)` when it doesn't start with a call).
fn head_function(src: &str) -> String {
    let t = src.trim_start_matches(['@', '+', '-', ' ']);
    let name: String = t
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_')
        .collect();
    let name = name
        .trim_start_matches("_xlfn.")
        .trim_start_matches("_xlws.")
        .to_string();
    if !name.is_empty() && t[name.len().min(t.len())..].contains('(') {
        name.to_uppercase()
    } else {
        "(expr)".into()
    }
}

/// Coarse first-mismatch pattern: expected kind vs got kind.
fn pattern(m: &Mismatch) -> String {
    let kind = |v: &str| {
        if v.starts_with('#') {
            v.to_string()
        } else if v.parse::<f64>().is_ok() {
            "num".to_string()
        } else if v == "TRUE" || v == "FALSE" {
            "bool".to_string()
        } else {
            "text".to_string()
        }
    };
    format!("{} <- {}", kind(&m.expected), kind(&m.got))
}

fn write_dump(path: &std::path::Path, rep: &OracleReport) -> Result<()> {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut out = String::from("file\tsheet\tcell\tfunction\tpattern\tformula\texpected\tgot\n");
    let mut groups: BTreeMap<(String, String), usize> = BTreeMap::new();
    for f in &rep.files {
        for m in &f.all_mismatches {
            let func = head_function(&m.formula);
            let pat = pattern(m);
            *groups.entry((func.clone(), pat.clone())).or_default() += 1;
            let _ = writeln!(
                out,
                "{}\t{}\t{}\t{func}\t{pat}\t{}\t{}\t{}",
                f.path,
                clean(&m.sheet),
                m.cell,
                clean(&m.formula),
                clean(&m.expected),
                clean(&m.got)
            );
        }
    }
    std::fs::write(path, out)?;
    let mut sorted: Vec<_> = groups.into_iter().collect();
    sorted.sort_by_key(|a| std::cmp::Reverse(a.1));
    println!("mismatches by function / pattern (expected <- got):");
    for ((func, pat), n) in sorted.iter().take(60) {
        println!("{n:>7}  {func:<16} {pat}");
    }
    println!("full list: {}", path.display());
    Ok(())
}

pub fn test(opts: &TestOpts) -> Result<()> {
    let verbose = opts.verbose;
    let dump = opts.dump.is_some();
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
        if let Some(only) = &opts.only
            && !e.path.contains(only.as_str())
        {
            continue;
        }
        let r = check_file(&local(e), e, dump);
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
    if let Some(path) = &opts.dump {
        write_dump(path, &rep)?;
    }
    if opts.only.is_none() {
        std::fs::write(
            crate::root().join("scorecard/oracle.json"),
            serde_json::to_string_pretty(&rep)?,
        )?;
    }
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
    if opts.only.is_some() {
        return Ok(());
    }
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
