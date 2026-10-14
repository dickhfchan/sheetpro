//! App settings (`settings.toml` in the app data directory) and the app data directory
//! itself (settings, autosave snapshots, logs, problem reports).
//!
//! The directory is resolved, in order, from: a per-thread override (tests), the directory
//! the desktop app registers at startup ([`init_app_dir`]), the `SHEETPRO_HOME` environment
//! variable. Library users (the CLI, panic_hunt) without any of these get `None`, and the
//! commands that need persistence return an error instead of touching the user's home.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Default gridline colour (the light theme's).
pub const DEFAULT_GRIDLINE: &str = "#DADCE0";
/// Most-recently-used list length cap.
pub const MAX_RECENT: usize = 25;
/// Largest settings file we read (anything bigger is treated as corrupt).
const MAX_SETTINGS_BYTES: u64 = 1 << 20;
/// Log file rotates past this size.
const MAX_LOG_BYTES: u64 = 2 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Most recent first.
    pub recent_files: Vec<String>,
    /// Seconds between autosave snapshots of a dirty workbook; 0 turns autosave off.
    pub autosave_interval_secs: u64,
    /// Font of the Normal style for new workbooks.
    pub default_font: String,
    pub default_font_size: f64,
    /// "automatic" or "manual": applied to new workbooks (and to the current one when set).
    pub calc_mode: String,
    /// R1C1 reference style: numbered column headings and R1C1 cell names in the Name Box
    /// (formula text is still shown in A1 style).
    pub r1c1: bool,
    /// Gridline colour as `#RRGGBB`.
    pub gridline_color: String,
    /// Recent-files list length.
    pub recent_count: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            recent_files: Vec::new(),
            autosave_interval_secs: 60,
            default_font: sp_model::style::DEFAULT_FONT.to_string(),
            default_font_size: 11.0,
            calc_mode: "automatic".into(),
            r1c1: false,
            gridline_color: DEFAULT_GRIDLINE.into(),
            recent_count: 10,
        }
    }
}

/// Keys `file.options` accepts, with a short description.
pub const KEYS: [(&str, &str); 7] = [
    (
        "autosave_interval_secs",
        "seconds between autosaves, 0 = off (max 86400)",
    ),
    ("default_font", "font name for new workbooks"),
    ("default_font_size", "font size for new workbooks (1-409)"),
    ("calc_mode", "\"automatic\" | \"manual\""),
    (
        "r1c1",
        "R1C1 reference style: numbered column headings, R1C1 Name Box (formulas stay A1)",
    ),
    ("gridline_color", "gridline colour, #RRGGBB"),
    ("recent_count", "recent-files list length (0-25)"),
];

impl Settings {
    /// Settings as JSON (what `file.options` returns).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    pub fn get(&self, key: &str) -> Result<serde_json::Value, String> {
        let all = self.to_json();
        all.get(key)
            .cloned()
            .ok_or_else(|| format!("unknown setting '{key}'"))
    }

    /// Validate and set one key.
    pub fn set(&mut self, key: &str, v: &serde_json::Value) -> Result<(), String> {
        let bad = || format!("invalid value for '{key}': {v}");
        match key {
            "autosave_interval_secs" => {
                let n = v.as_u64().filter(|n| *n <= 86_400).ok_or_else(bad)?;
                self.autosave_interval_secs = n;
            }
            "default_font" => {
                let s = v
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.chars().count() <= 64)
                    .ok_or_else(bad)?;
                self.default_font = s.to_string();
            }
            "default_font_size" => {
                let n = v
                    .as_f64()
                    .filter(|n| n.is_finite() && (1.0..=409.0).contains(n))
                    .ok_or_else(bad)?;
                self.default_font_size = (n * 2.0).round() / 2.0;
            }
            "calc_mode" => {
                let s = v
                    .as_str()
                    .map(str::to_ascii_lowercase)
                    .filter(|s| s == "automatic" || s == "manual")
                    .ok_or_else(bad)?;
                self.calc_mode = s;
            }
            "r1c1" => self.r1c1 = v.as_bool().ok_or_else(bad)?,
            "gridline_color" => {
                let [r, g, b] = v.as_str().and_then(parse_hex_rgb).ok_or_else(bad)?;
                self.gridline_color = format!("#{r:02X}{g:02X}{b:02X}");
            }
            "recent_count" => {
                let n = v
                    .as_u64()
                    .filter(|n| *n <= MAX_RECENT as u64)
                    .ok_or_else(bad)?;
                self.recent_count = usize::try_from(n).map_err(|_| bad())?;
                self.recent_files.truncate(self.recent_count);
            }
            _ => return Err(format!("unknown setting '{key}'")),
        }
        Ok(())
    }

    /// Put `path` at the front of the recent list (deduplicated, capped).
    pub fn push_recent(&mut self, path: &Path) {
        let p = path.display().to_string();
        self.recent_files.retain(|x| *x != p);
        self.recent_files.insert(0, p);
        self.recent_files
            .truncate(self.recent_count.min(MAX_RECENT));
    }

    /// The gridline colour as RGB, if it parses.
    pub fn gridline_rgb(&self) -> Option<[u8; 3]> {
        parse_hex_rgb(&self.gridline_color)
    }

    /// Clamp values a hand-edited file may have broken.
    fn sanitize(mut self) -> Self {
        let d = Settings::default();
        if self.autosave_interval_secs > 86_400 {
            self.autosave_interval_secs = d.autosave_interval_secs;
        }
        if !(self.default_font_size.is_finite() && (1.0..=409.0).contains(&self.default_font_size))
        {
            self.default_font_size = d.default_font_size;
        }
        if self.default_font.trim().is_empty() {
            self.default_font = d.default_font;
        }
        if self.calc_mode != "automatic" && self.calc_mode != "manual" {
            self.calc_mode = d.calc_mode;
        }
        if parse_hex_rgb(&self.gridline_color).is_none() {
            self.gridline_color = d.gridline_color;
        }
        self.recent_count = self.recent_count.min(MAX_RECENT);
        self.recent_files.truncate(self.recent_count);
        self
    }

    /// Load from `dir/settings.toml`; a missing or unreadable file gives the defaults.
    pub fn load(dir: &Path) -> Settings {
        let path = dir.join("settings.toml");
        let ok_size = std::fs::metadata(&path).is_ok_and(|m| m.len() <= MAX_SETTINGS_BYTES);
        if !ok_size {
            return Settings::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str::<Settings>(&t).ok())
            .map(Settings::sanitize)
            .unwrap_or_default()
    }

    /// Write `dir/settings.toml` atomically.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        write_atomic(&dir.join("settings.toml"), text.as_bytes())
    }

    /// Apply the new-workbook defaults (Normal font, calc mode) to a fresh session without
    /// marking it dirty or adding an undo step.
    pub fn apply_to_new(&self, s: &mut crate::Session) {
        let style = sp_model::style::Style {
            font_name: self.default_font.clone(),
            font_size_tenths: (self.default_font_size * 10.0).round().clamp(10.0, 4090.0) as u32,
            ..Default::default()
        };
        if style != sp_model::style::Style::default()
            && let Ok(table) = serde_json::from_value::<sp_model::style::StyleTable>(
                serde_json::json!({ "styles": [style] }),
            )
            && s.wb.styles.len() <= 1
        {
            s.wb.styles = table;
        }
        s.wb.calc_mode = if self.calc_mode == "manual" {
            sp_model::CalcMode::Manual
        } else {
            sp_model::CalcMode::Automatic
        };
    }
}

/// `#RRGGBB` (or `RRGGBB`) as RGB.
pub fn parse_hex_rgb(s: &str) -> Option<[u8; 3]> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|b| u8::from_str_radix(b, 16).ok())
    };
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// Write `bytes` to `path` via a sibling temp file and a rename (creating parent dirs).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let r = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------
// App data directory

static APP_DIR: OnceLock<PathBuf> = OnceLock::new();

thread_local! {
    static OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// The platform's per-user app data directory for SheetPro (not created).
/// macOS: `~/Library/Application Support/SheetPro`; Windows: `%APPDATA%\SheetPro`;
/// others: `$XDG_CONFIG_HOME/sheetpro` or `~/.config/sheetpro`.
pub fn platform_app_dir() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/SheetPro"))
    } else if cfg!(windows) {
        env("APPDATA").map(|h| h.join("SheetPro"))
    } else {
        env("XDG_CONFIG_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".config")))
            .map(|h| h.join("sheetpro"))
    }
}

/// Register the app data directory for this process (the desktop app calls this once).
pub fn init_app_dir(dir: PathBuf) {
    let _ = APP_DIR.set(dir);
}

/// Use `dir` as the app data directory on this thread only (tests); `None` clears.
pub fn set_thread_app_dir(dir: Option<PathBuf>) {
    OVERRIDE.with(|o| *o.borrow_mut() = dir);
}

/// The app data directory, if one is configured (see the module docs).
pub fn app_dir() -> Option<PathBuf> {
    if let Some(d) = OVERRIDE.with(|o| o.borrow().clone()) {
        return Some(d);
    }
    if let Some(d) = APP_DIR.get() {
        return Some(d.clone());
    }
    std::env::var_os("SHEETPRO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `app_dir()` or a command error explaining why persistence is unavailable.
pub fn require_app_dir() -> Result<PathBuf, crate::CmdError> {
    app_dir().ok_or_else(|| {
        crate::CmdError::Failed(
            "no app data directory (run the desktop app or set SHEETPRO_HOME)".into(),
        )
    })
}

/// Load settings from the app dir (defaults when there is none).
pub fn current() -> Settings {
    app_dir().map(|d| Settings::load(&d)).unwrap_or_default()
}

/// Settings from the app dir, or `fallback` when no app dir is configured (so in-memory
/// preferences survive in sessions without persistence).
pub fn current_or(fallback: &Settings) -> Settings {
    app_dir()
        .map(|d| Settings::load(&d))
        .unwrap_or_else(|| fallback.clone())
}

/// Load, change, save.
pub fn update(
    f: impl FnOnce(&mut Settings) -> Result<(), String>,
) -> Result<Settings, crate::CmdError> {
    let dir = require_app_dir()?;
    let mut s = Settings::load(&dir);
    f(&mut s).map_err(crate::CmdError::Param)?;
    s.save(&dir).map_err(crate::CmdError::Failed)?;
    Ok(s)
}

// ---------------------------------------------------------------------------------------
// Recent command log (for problem reports): command ids and outcomes, never params or
// cell contents. Per thread, bounded.

const MAX_COMMAND_LOG: usize = 200;

thread_local! {
    static COMMAND_LOG: RefCell<std::collections::VecDeque<String>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
}

/// Record that command `id` ran (`Session::execute` calls this). Navigation and selection
/// commands are skipped: they would push everything else out.
pub fn record_command(id: &str, ok: bool) {
    if id.starts_with("nav.") || id.starts_with("select.") {
        return;
    }
    let line = format!("{} {id}{}", unix_now(), if ok { "" } else { " (failed)" });
    COMMAND_LOG.with(|l| {
        let mut l = l.borrow_mut();
        while l.len() >= MAX_COMMAND_LOG {
            l.pop_front();
        }
        l.push_back(line);
    });
}

/// The recent command log of this thread, oldest first, one command per line.
pub fn command_log() -> Vec<String> {
    COMMAND_LOG.with(|l| l.borrow().iter().cloned().collect())
}

pub fn log_path(dir: &Path) -> PathBuf {
    dir.join("logs").join("sheetpro.log")
}

/// Append one timestamped line to the app log (rotating to `sheetpro.log.1` when large).
/// Logging never fails loudly.
pub fn log_line(msg: &str) {
    let Some(dir) = app_dir() else {
        return;
    };
    let path = log_path(&dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let one_line: String = msg
            .chars()
            .map(|c| if c == '\n' { ' ' } else { c })
            .collect();
        let _ = writeln!(f, "{} {}", unix_now(), one_line);
    }
}

/// The last `max_bytes` of the app log (lossy UTF-8), or an empty string.
pub fn log_tail(dir: &Path, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(log_path(dir)) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(max_bytes).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Seconds since the Unix epoch (0 if the clock is before it).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sp-settings-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn roundtrip_and_validation() {
        let dir = temp_dir("rt");
        assert_eq!(Settings::load(&dir), Settings::default());
        let mut s = Settings::default();
        s.set("autosave_interval_secs", &json!(30)).unwrap();
        s.set("default_font", &json!("Arial")).unwrap();
        s.set("default_font_size", &json!(12.25)).unwrap();
        s.set("calc_mode", &json!("Manual")).unwrap();
        s.set("r1c1", &json!(true)).unwrap();
        s.set("gridline_color", &json!("#a0b0c0")).unwrap();
        assert!(s.set("autosave_interval_secs", &json!(-1)).is_err());
        assert!(s.set("default_font_size", &json!(1000)).is_err());
        assert!(s.set("calc_mode", &json!("sometimes")).is_err());
        assert!(s.set("gridline_color", &json!("red")).is_err());
        assert!(s.set("nope", &json!(1)).is_err());
        assert_eq!(s.default_font_size, 12.5);
        assert_eq!(s.gridline_color, "#A0B0C0");
        s.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), s);
        assert_eq!(s.get("calc_mode").unwrap(), json!("manual"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_gives_defaults_and_sanitizes() {
        let dir = temp_dir("bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.toml"), "this is = = not toml").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        std::fs::write(
            dir.join("settings.toml"),
            "default_font_size = -5.0\ncalc_mode = \"x\"\nrecent_count = 999\n",
        )
        .unwrap();
        let s = Settings::load(&dir);
        assert_eq!(s.default_font_size, 11.0);
        assert_eq!(s.calc_mode, "automatic");
        assert_eq!(s.recent_count, MAX_RECENT);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_is_mru_dedup_capped() {
        let mut s = Settings::default();
        s.set("recent_count", &json!(3)).unwrap();
        for p in ["/a.xlsx", "/b.xlsx", "/a.xlsx", "/c.xlsx", "/d.xlsx"] {
            s.push_recent(Path::new(p));
        }
        assert_eq!(s.recent_files, vec!["/d.xlsx", "/c.xlsx", "/a.xlsx"]);
    }

    #[test]
    fn defaults_apply_to_new_workbooks() {
        let mut st = Settings::default();
        st.set("default_font", &json!("Arial")).unwrap();
        st.set("default_font_size", &json!(14)).unwrap();
        st.set("calc_mode", &json!("manual")).unwrap();
        let mut s = crate::Session::new();
        st.apply_to_new(&mut s);
        let d = s.wb.styles.get(sp_model::style::StyleId::default());
        assert_eq!(d.font_name, "Arial");
        assert_eq!(d.font_size_tenths, 140);
        assert_eq!(s.wb.calc_mode, sp_model::CalcMode::Manual);
        assert!(!s.dirty);
    }

    #[test]
    fn command_log_is_bounded_and_skips_navigation() {
        for i in 0..(MAX_COMMAND_LOG + 5) {
            record_command(&format!("cmd.{i}"), i % 2 == 0);
        }
        record_command("nav.down", true);
        let log = command_log();
        assert_eq!(log.len(), MAX_COMMAND_LOG);
        assert!(
            log.last()
                .unwrap()
                .ends_with(&format!("cmd.{}", MAX_COMMAND_LOG + 4))
        );
        assert!(log.iter().any(|l| l.ends_with("(failed)")));
        assert!(!log.iter().any(|l| l.contains("nav.")));
    }

    #[test]
    fn gridline_colour_parses_and_sanitizes() {
        assert_eq!(parse_hex_rgb("#DADCE0"), Some([0xDA, 0xDC, 0xE0]));
        assert_eq!(parse_hex_rgb("zzzzzz"), None);
        assert_eq!(parse_hex_rgb("#ÄÄÄ"), None);
        assert_eq!(Settings::default().gridline_rgb(), Some([0xDA, 0xDC, 0xE0]));
        let dir = temp_dir("grid");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.toml"), "gridline_color = \"blue\"\n").unwrap();
        assert_eq!(Settings::load(&dir).gridline_color, DEFAULT_GRIDLINE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_appends_and_tails() {
        let dir = temp_dir("log");
        set_thread_app_dir(Some(dir.clone()));
        log_line("hello\nworld");
        log_line("second");
        let t = log_tail(&dir, 1000);
        assert!(t.contains("hello world") && t.ends_with("second\n"));
        assert!(log_tail(&dir, 7).len() <= 7);
        set_thread_app_dir(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
